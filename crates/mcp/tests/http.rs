mod common;
use mcp::{Session, host::Host};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn authenticated_http_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = common::write_fixture(dir.path());
    let mut session = Session::new(false);
    session
        .open_project(mcp::session::OpenArgs {
            path: path.display().to_string(),
        })
        .unwrap();
    let host: Arc<dyn Host> = Arc::new(Mutex::new(session));
    let token = mcp::http::token(&dir.path().join("token"), false).unwrap();
    let server = mcp::http::start(0, token.clone(), false, host).unwrap();
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/mcp", server.port);
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}});
    assert_eq!(
        client.post(&url).json(&init).send().await.unwrap().status(),
        401
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&token)
            .header("origin", "http://evil.example")
            .json(&init)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(&url)
        .bearer_auth(&token)
        .header("accept", "application/json, text/event-stream")
        .json(&init)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let session = response.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let value: Value = decode(response).await;
    assert!(value["result"]["capabilities"]["tools"].is_object());
    let initialized = client
        .post(&url)
        .bearer_auth(&token)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session)
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert!(initialized.status().is_success());
    for (method, params) in [
        ("tools/list", json!({})),
        (
            "tools/call",
            json!({"name":"project_summary","arguments":{}}),
        ),
    ] {
        let response = client
            .post(&url)
            .bearer_auth(&token)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-session-id", &session)
            .json(&json!({"jsonrpc":"2.0","id":2,"method":method,"params":params}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let value: Value = decode(response).await;
        assert!(value.get("error").is_none(), "{value}");
        if method == "tools/list" {
            assert!(
                value["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|t| t["name"] == "project_summary")
            );
            assert!(
                !value["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|t| t["name"] == "transform_object")
            );
        } else {
            assert_eq!(
                value["result"]["structuredContent"]["plates"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        } // No open project in this host.
    }
}

#[test]
fn tool_layer_through_host() {
    let host: Arc<dyn Host> = Arc::new(Mutex::new(Session::new(false)));
    let result = host
        .call(
            "explain_setting",
            Box::new(|s| {
                s.explain_setting(mcp::session::ExplainArgs {
                    key: "layer_height".into(),
                })
            }),
        )
        .unwrap();
    assert_eq!(result["key"], "layer_height");
    assert!(host.call("undo", Box::new(|s| s.undo())).is_err());
}

#[test]
fn host_edits_share_state_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let path = common::write_fixture(dir.path());
    let host: Arc<dyn Host> = Arc::new(Mutex::new(Session::new(true)));
    host.call(
        "open_project",
        Box::new(move |s| {
            s.open_project(mcp::session::OpenArgs {
                path: path.display().to_string(),
            })
        }),
    )
    .unwrap();
    let before = host
        .call("project_summary", Box::new(|s| s.project_summary()))
        .unwrap();
    host.call(
        "transform_object",
        Box::new(|s| {
            s.transform_object(
                serde_json::from_value(json!({"object":1,"scale":[0.5,0.5,0.5]})).unwrap(),
            )
        }),
    )
    .unwrap();
    let after = host
        .call("project_summary", Box::new(|s| s.project_summary()))
        .unwrap();
    assert_ne!(before["plates"], after["plates"]);
    host.call("undo", Box::new(|s| s.undo())).unwrap();
    let restored = host
        .call("project_summary", Box::new(|s| s.project_summary()))
        .unwrap();
    assert_eq!(before["plates"], restored["plates"]);
}

/// GUI-хост: `ui_state` є в `tools/list` і віддає знімок стану вікна як є.
struct UiHost(Mutex<Session>);
impl Host for UiHost {
    fn has_ui(&self) -> bool {
        true
    }
    fn ui_state(&self) -> anyhow::Result<Value> {
        Ok(json!({"page": "prepare", "camera": [0.5, 0.3, 300.0, 128.0, 128.0, 0.0]}))
    }
    fn call(&self, name: &'static str, op: mcp::host::Operation) -> anyhow::Result<Value> {
        self.0.call(name, op)
    }
    fn prepare_slice(
        &self,
        args: mcp::session::SliceArgs,
    ) -> anyhow::Result<mcp::session::SliceJob> {
        self.0.prepare_slice(args)
    }
}

#[tokio::test]
async fn ui_state_only_on_gui_hosts() {
    for (host, listed) in [
        (Arc::new(Mutex::new(Session::new(false))) as Arc<dyn Host>, false),
        (Arc::new(UiHost(Mutex::new(Session::new(false)))) as Arc<dyn Host>, true),
    ] {
        let server = mcp::http::start(0, "token".into(), false, host).unwrap();
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/mcp", server.port);
        let post = |body: Value, session: Option<String>| {
            let mut request = client
                .post(&url)
                .bearer_auth("token")
                .header("accept", "application/json, text/event-stream");
            if let Some(session) = session {
                request = request.header("mcp-session-id", session);
            }
            request.json(&body).send()
        };
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}});
        let response = post(init, None).await.unwrap();
        let session = response.headers()["mcp-session-id"].to_str().unwrap().to_owned();
        post(json!({"jsonrpc":"2.0","method":"notifications/initialized"}), Some(session.clone()))
            .await
            .unwrap();
        let list = decode(
            post(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}), Some(session.clone()))
                .await
                .unwrap(),
        )
        .await;
        let tools = list["result"]["tools"].as_array().unwrap();
        let tool = tools.iter().find(|t| t["name"] == "ui_state");
        assert_eq!(tool.is_some(), listed, "{list}");
        if let Some(tool) = tool {
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
            let reply = decode(
                post(
                    json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ui_state","arguments":{}}}),
                    Some(session),
                )
                .await
                .unwrap(),
            )
            .await;
            assert_eq!(reply["result"]["structuredContent"]["page"], "prepare", "{reply}");
        }
    }
}

#[test]
fn restart_releases_port_and_rotates_authorization() {
    let host: Arc<dyn Host> = Arc::new(Mutex::new(Session::new(false)));
    let server = mcp::http::start(0, "first".into(), false, host.clone()).unwrap();
    let port = server.port;
    drop(server);
    let replacement = mcp::http::start(port, "second".into(), true, host).unwrap();
    assert_eq!(replacement.port, port);
}

async fn decode(response: reqwest::Response) -> Value {
    let body = response.text().await.unwrap();
    if let Ok(value) = serde_json::from_str(&body) {
        return value;
    }
    let data = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .find(|data| !data.trim().is_empty())
        .expect(&body);
    serde_json::from_str(data).expect(&body)
}
