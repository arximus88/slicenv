//! End-to-end: справжній процес `slicenv-mcp` по stdio — initialize → tools/list → tools/call.
mod common;

use std::{
    io::{BufRead as _, BufReader, Write as _},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{Receiver, channel},
    time::Duration,
};

use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next: u64,
}

impl Client {
    fn start(args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_slicenv-mcp"))
            .args(args)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let mut c = Self {
            child,
            stdin,
            lines,
            next: 0,
        };
        let init = c.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "slicenv");
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(60))
                .expect("сервер мовчить");
            // Кожен рядок stdout — валідний JSON-RPC; сторонній вивід зіпсував би протокол.
            let message: Value =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("не JSON: {line:?}: {e}"));
            if message["id"] == id {
                return message;
            }
        }
    }

    fn tool_names(&mut self) -> Vec<String> {
        let list = self.request("tools/list", json!({}));
        let mut names: Vec<String> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        names.sort();
        names
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": tool, "arguments": arguments}))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn read_only_server_lists_only_read_tools_and_summarizes_a_project() {
    let dir = tempfile::tempdir().unwrap();
    let path = common::write_fixture(dir.path());
    let mut c = Client::start(&[], &[]);

    let names = c.tool_names();
    for read in [
        "open_project",
        "project_summary",
        "list_profiles",
        "get_profile",
        "explain_setting",
        "gcode_stats",
        "slice_plate",
        "printer_status",
    ] {
        assert!(names.contains(&read.to_owned()), "немає {read}: {names:?}");
    }
    for edit in [
        "transform_object",
        "arrange_plate",
        "undo",
        "redo",
        "save_project",
        // Без вікна стану UI немає.
        "ui_state",
    ] {
        assert!(
            !names.contains(&edit.to_owned()),
            "{edit} має бути відсутній без --allow-edit"
        );
    }

    let reply = c.call("project_summary", json!({}));
    assert_eq!(
        reply["result"]["isError"], true,
        "до open_project проєкту немає"
    );

    c.call("open_project", json!({"path": path}));
    let reply = c.call("project_summary", json!({}));
    assert_eq!(reply["result"]["isError"], false);
    let plates = &reply["result"]["structuredContent"]["plates"];
    assert_eq!(plates[0]["objects"].as_array().unwrap().len(), 2);
    assert_eq!(
        plates[0]["objects"][0]["size_mm"],
        json!([20.0, 20.0, 20.0])
    );

    // Виклик відсутнього інструмента (зміни) — помилка протоколу, не виконання.
    let reply = c.call(
        "transform_object",
        json!({"object": 1, "translate": [1, 0, 0]}),
    );
    assert!(
        reply["error"].is_object() || reply["result"]["isError"] == true,
        "{reply}"
    );
}

#[test]
fn allow_edit_server_edits_with_undo() {
    let dir = tempfile::tempdir().unwrap();
    let path = common::write_fixture(dir.path());
    let mut c = Client::start(&["--allow-edit"], &[]);
    assert!(c.tool_names().contains(&"transform_object".to_owned()));

    c.call("open_project", json!({"path": path}));
    let moved = c.call(
        "transform_object",
        json!({"object": 1, "translate": [0, 10, 0]}),
    );
    assert_eq!(moved["result"]["structuredContent"]["bounds_min"][1], 50.0);
    c.call("undo", json!({}));
    let summary = c.call("project_summary", json!({}));
    assert_eq!(
        summary["result"]["structuredContent"]["plates"][0]["objects"][0]["bounds_min"][1],
        40.0
    );
}

#[test]
fn printer_status_without_credentials_is_a_clear_error() {
    let mut c = Client::start(&[], &[("BAMBU_STUDIO_CONFIG", "/nonexistent/bambu-studio")]);
    let reply = c.call("printer_status", json!({}));
    assert_eq!(reply["result"]["isError"], true);
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Bambu Studio"), "{text}");
}

#[test]
fn summarizes_a_real_project_when_present() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let path = std::path::Path::new(&home).join("Downloads/Phone+Stand+v3.3mf");
    if !path.is_file() {
        eprintln!("пропущено: немає {}", path.display());
        return;
    }
    let mut c = Client::start(&[], &[]);
    let reply = c.call("open_project", json!({"path": path}));
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let object = &reply["result"]["structuredContent"]["plates"][0]["objects"][0];
    assert!(object["triangles"].as_u64().unwrap() > 1000);
    assert!(object["size_mm"][2].as_f64().unwrap() > 10.0);
}
