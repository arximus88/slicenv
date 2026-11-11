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
        Self::start_with(args, env, &[])
    }

    fn start_with(args: &[&str], env: &[(&str, &str)], remove: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slicenv-mcp"));
        for key in remove {
            command.env_remove(key);
        }
        let mut child = command
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

    /// Без змінних, якими можна вказати рушій чи профілі напряму: так видно, що сервер сам
    /// знаходить рушій.
    fn start_clean(args: &[&str], env: &[(&str, &str)]) -> Self {
        Self::start_with(args, env, &["BAMBU_STUDIO", "ORCA_SLICER", "SLICENV_PROFILE_ROOT", "ORCA_PROFILE_ROOT"])
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

/// Фіктивний рушій, встановлений менеджером Slicenv: без власних JSON-профілів (як Orca 2.5
/// з `.opc`), профілі — у бібліотеці `<data>/slicenv/profiles/orca/<версія>`.
fn fake_managed_orca(data: &std::path::Path, version: &str) -> std::path::PathBuf {
    use std::fs;
    let root = data.join("slicenv/engines/orca").join(version);
    fs::create_dir_all(root.join("bin")).unwrap();
    let exe = root.join("bin/orca-slicer");
    fs::write(&exe, "#!/bin/sh\nexit 1\n").unwrap();
    fs::write(
        root.join("manifest.json"),
        json!({"managed": true, "family": "orca", "version": version,
               "executable_path": exe}).to_string(),
    )
    .unwrap();
    let catalog = data.join("slicenv/profiles/orca").join(version);
    for (dir, name, extra) in [
        ("machine", "Fake Printer 0.4 nozzle", json!({"printable_area": ["0x0", "180x0", "180x180", "0x180"]})),
        ("process", "0.20mm Fake", json!({"compatible_printers": ["Fake Printer 0.4 nozzle"]})),
        ("filament", "Fake PLA", json!({"compatible_printers": ["Fake Printer 0.4 nozzle"]})),
    ] {
        let path = catalog.join("BBL").join(dir);
        fs::create_dir_all(&path).unwrap();
        let mut value = json!({"name": name, "type": dir, "instantiation": "true", "from": "system"});
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        fs::write(path.join(format!("{name}.json")), value.to_string()).unwrap();
    }
    fs::write(catalog.join("BBL.json"), json!({"name": "BBL", "version": "1"}).to_string()).unwrap();
    fs::write(
        catalog.join("catalog.json"),
        json!({"family": "orca", "version": version, "vendors": ["BBL"]}).to_string(),
    )
    .unwrap();
    exe
}

/// Раніше `slicenv-mcp` шукав рушій лише в PATH і системних теках, тож керовані рушії
/// й бібліотека профілів були йому невидимі, хоч застосунок їх бачив.
#[test]
fn sees_profiles_of_engine_installed_by_manager() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    // Корінь даних там, де його шукає `paths::data_home` на цій ОС: `$XDG_DATA_HOME` на Linux,
    // `~/Library/Application Support` на macOS, `%LOCALAPPDATA%` на Windows.
    let data = if cfg!(target_os = "macos") {
        h.join("Library/Application Support")
    } else if cfg!(windows) {
        h.join("Local")
    } else {
        h.join("d")
    };
    fake_managed_orca(&data, "2.5.0");
    let empty_path = h.join("empty-bin");
    std::fs::create_dir_all(&empty_path).unwrap();
    let env = [
        ("HOME", h.to_str().unwrap().to_owned()),
        ("XDG_CONFIG_HOME", h.join("c").to_str().unwrap().to_owned()),
        ("XDG_DATA_HOME", data.to_str().unwrap().to_owned()),
        ("XDG_CACHE_HOME", h.join("k").to_str().unwrap().to_owned()),
        ("XDG_STATE_HOME", h.join("s").to_str().unwrap().to_owned()),
        ("USERPROFILE", h.to_str().unwrap().to_owned()),
        ("APPDATA", h.join("Roaming").to_str().unwrap().to_owned()),
        ("LOCALAPPDATA", h.join("Local").to_str().unwrap().to_owned()),
        // Без системних рушіїв з PATH (на машині розробника є /usr/bin/bambustudio).
        ("PATH", empty_path.to_str().unwrap().to_owned()),
        ("SLICENV_CLOUD_OFF", "1".into()),
        ("SLICENV_LAN_OFF", "1".into()),
    ];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut c = Client::start_clean(&[], &env);

    let machines = c.call("list_profiles", json!({"kind": "machine"}));
    let names = &machines["result"]["structuredContent"]["names"];
    assert!(
        names.as_array().is_some_and(|n| n.contains(&json!("Fake Printer 0.4 nozzle"))),
        "{machines}"
    );
    for kind in ["process", "filament"] {
        let reply = c.call(
            "list_profiles",
            json!({"kind": kind, "machine": "Fake Printer 0.4 nozzle"}),
        );
        assert_eq!(reply["result"]["structuredContent"]["count"], 1, "{kind}: {reply}");
    }
    let profile = c.call(
        "get_profile",
        json!({"kind": "machine", "name": "Fake Printer 0.4 nozzle", "keys": ["printable_area"]}),
    );
    assert_eq!(
        profile["result"]["structuredContent"]["values"]["printable_area"],
        json!(["0x0", "180x0", "180x180", "0x180"]),
        "{profile}"
    );
}
