//! Loopback-only Streamable HTTP with bearer authentication before MCP session handling.
use crate::{Server, host::Host};
use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::Response,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use std::future::IntoFuture;
use std::{
    net::{Ipv4Addr, TcpListener},
    path::Path,
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

pub struct HttpServer {
    pub port: u16,
    cancel: CancellationToken,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for HttpServer {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn start(
    port: u16,
    token: String,
    allow_edit: bool,
    host: Arc<dyn Host>,
) -> Result<HttpServer> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let runtime = tokio::runtime::Runtime::new()?;
    let thread = std::thread::Builder::new()
        .name("mcp-http".into())
        .spawn(move || {
            runtime.block_on(async move {
                let mut config = StreamableHttpServerConfig::default();
                config.cancellation_token = shutdown.clone();
                config.json_response = true;
                let service = StreamableHttpService::new(
                    move || Ok(Server::with_host(allow_edit, host.clone())),
                    Arc::new(LocalSessionManager::default()),
                    config,
                );
                let expected = format!("Bearer {token}");
                let router =
                    Router::new()
                        .nest_service("/mcp", service)
                        .layer(middleware::from_fn(
                            move |req: Request<Body>, next: Next| {
                                let expected = expected.clone();
                                async move {
                                    if req
                                        .headers()
                                        .get_all("origin")
                                        .iter()
                                        .any(|v| !v.to_str().is_ok_and(loopback_origin))
                                    {
                                        return Response::builder()
                                            .status(StatusCode::FORBIDDEN)
                                            .body(Body::empty())
                                            .unwrap();
                                    }
                                    if req
                                        .headers()
                                        .get("authorization")
                                        .and_then(|v| v.to_str().ok())
                                        != Some(expected.as_str())
                                    {
                                        return Response::builder()
                                            .status(StatusCode::UNAUTHORIZED)
                                            .header("www-authenticate", "Bearer")
                                            .body(Body::empty())
                                            .unwrap();
                                    }
                                    next.run(req).await
                                }
                            },
                        ));
                let listener =
                    tokio::net::TcpListener::from_std(listener).expect("validated listener");
                tokio::select! {
                    _ = shutdown.cancelled() => {},
                    _ = axum::serve(listener, router).into_future() => {},
                }
            });
            runtime.shutdown_background();
        })?;
    Ok(HttpServer {
        port,
        cancel,
        thread: Some(thread),
    })
}
fn loopback_origin(origin: &str) -> bool {
    url::Url::parse(origin).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.username().is_empty()
            && u.password().is_none()
            && match u.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                Some(url::Host::Domain(name)) => name == "localhost",
                None => false,
            }
    })
}
pub fn token(path: &Path, regenerate: bool) -> Result<String> {
    if !regenerate && path.exists() {
        let value = std::fs::read_to_string(path)?;
        ensure!(
            value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
            "некоректний MCP-токен"
        );
        paths::fs_mode::set_mode(path, 0o600)?;
        return Ok(value);
    }
    let parent = path.parent().context("немає каталогу конфігурації")?;
    std::fs::create_dir_all(parent)?;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("генерація токена: {e}"))?;
    let value: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    paths::fs_mode::set_file_mode(file.as_file(), 0o600)?;
    use std::io::Write;
    file.write_all(value.as_bytes())?;
    file.persist(path)?;
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origins_and_tokens() {
        for value in ["http://127.0.0.1:7357", "http://localhost", "https://[::1]"] {
            assert!(loopback_origin(value));
        }
        for value in [
            "null",
            "http://evil.test",
            "http://localhost.evil.test",
            "http://127.0.0.1@evil.test",
        ] {
            assert!(!loopback_origin(value));
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let a = token(&path, false).unwrap();
        assert_eq!(a, token(&path, false).unwrap());
        assert_ne!(a, token(&path, true).unwrap());
        #[cfg(unix)]
        assert_eq!(
            paths::fs_mode::mode(&std::fs::metadata(path).unwrap()),
            Some(0o600)
        );
    }
}
