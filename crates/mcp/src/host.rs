//! Transport-independent execution boundary. GUI hosts marshal these jobs to GPUI.
use crate::session::{Session, SliceArgs, SliceJob};
use anyhow::Result;
use serde_json::Value;
use std::sync::Mutex;
pub type Operation = Box<dyn FnOnce(&mut Session) -> Result<Value> + Send>;
pub trait Host: Send + Sync + 'static {
    fn open_is_edit(&self) -> bool {
        false
    }
    /// Чи є в хоста вікно, стан якого віддає `ui_state`. Без нього інструмента немає в `tools/list`.
    fn has_ui(&self) -> bool {
        false
    }
    /// Read-only знімок стану GUI (той самий JSON, що `SLICENV_UI_TRACE`).
    fn ui_state(&self) -> Result<Value> {
        anyhow::bail!("ui_state доступний лише в застосунку Slicenv")
    }
    fn call(&self, name: &'static str, operation: Operation) -> Result<Value>;
    fn prepare_slice(&self, args: SliceArgs) -> Result<SliceJob>;
    /// Нарізка цілком (блокує викликача до завершення). GUI-хост запускає її у
    /// власному реєстрі фонових задач, щоб скасування й застарілість були спільні.
    fn slice(&self, args: SliceArgs) -> Result<Value> {
        self.prepare_slice(args)?.run()
    }
}
impl Host for Mutex<Session> {
    fn call(&self, _: &'static str, operation: Operation) -> Result<Value> {
        operation(
            &mut *self
                .lock()
                .map_err(|_| anyhow::anyhow!("сесію пошкоджено"))?,
        )
    }
    fn prepare_slice(&self, args: SliceArgs) -> Result<SliceJob> {
        self.lock()
            .map_err(|_| anyhow::anyhow!("сесію пошкоджено"))?
            .prepare_slice(args)
    }
}
