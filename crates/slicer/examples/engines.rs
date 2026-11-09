use anyhow::Result;
use slicer::{Engine, EngineKind};

fn main() -> Result<()> {
    for kind in [EngineKind::Bambu, EngineKind::Orca] {
        match Engine::discover(kind).and_then(|engine| engine.probe().map(|info| (engine, info))) {
            Ok((engine, info)) => println!(
                "{}: {} ({})",
                kind.name(),
                engine.path.display(),
                info.version
            ),
            Err(error) => println!("{}: unavailable ({error:#})", kind.name()),
        }
    }
    Ok(())
}
