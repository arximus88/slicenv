//! MCP-сервер для Slicenv: AI-агенти читають і редагують проєкт та нарізають його.
//!
//! * [`session`] — шар інструментів (чистий Rust, без MCP);
//! * [`server`] — тонка обгортка `rmcp`: розкладає `tools/call` на методи сесії;
//! * [`status`] — стан принтера з хмари (лише читання).

pub mod server;
pub mod session;
mod status;

pub use server::Server;
pub use session::Session;
pub mod host;
pub mod http;
