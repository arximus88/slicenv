use anyhow::Result;
use serde_json::{Map, Value, json};
use settings::{A1_PRESETS, Schema};
use slicer::PresetStore;

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let fixtures = if first.as_deref() == Some(std::ffi::OsStr::new("--fixtures")) {
        Some(std::path::PathBuf::from(
            args.next().expect("--fixtures requires a directory"),
        ))
    } else {
        None
    };
    let store = match if fixtures.is_some() {
        args.next()
    } else {
        first
    } {
        Some(root) => PresetStore::open(std::path::Path::new(&root))?,
        None => PresetStore::open_default()?,
    };
    let schema = Schema::load()?;
    let mut presets = Map::new();
    for &(kind, name) in A1_PRESETS {
        let values = store.flatten(kind.into(), name)?;
        if let Some(directory) = &fixtures {
            std::fs::create_dir_all(directory)?;
            let index = A1_PRESETS.iter().position(|p| *p == (kind, name)).unwrap();
            std::fs::write(
                directory.join(format!("a1-{index}.json")),
                serde_json::to_vec_pretty(&values)?,
            )?;
        }
        let mut roundtrips = 0;
        for (key, raw) in &values {
            let parsed = schema.parse(key, raw)?;
            anyhow::ensure!(parsed.to_json() == *raw, "roundtrip changed {name}/{key}");
            roundtrips += 1;
        }
        let mut coverage = serde_json::to_value(schema.coverage(&values))?;
        coverage["roundtrips"] = json!(roundtrips);
        let compatibility: Vec<_> = values
            .keys()
            .filter(|key| {
                !settings::is_metadata_key(key)
                    && schema
                        .options
                        .get(*key)
                        .is_some_and(|option| option.origin == "bambu_compatibility")
            })
            .collect();
        coverage["primary_covered"] =
            json!(coverage["covered"].as_u64().unwrap() - compatibility.len() as u64);
        coverage["compatibility_keys"] = json!(compatibility);
        presets.insert(name.into(), coverage);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "presets": Value::Object(presets), "missing_layout_keys": schema.missing_layout_keys(),
        }))?
    );
    Ok(())
}
