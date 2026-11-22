use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

fn inspector() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_inkue-inspect"));
    command.arg(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../src-tauri/resources/rehearsal/Operator rehearsal.inkue"),
    );
    command
}

#[test]
fn inspect_a_saved_list_and_cart_without_a_running_desk() {
    let result = inspector().arg("summary").output().unwrap();
    assert!(result.status.success());
    let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(summary["source"], "saved_workspace");
    assert_eq!(summary["lists"][1]["mode"], "cart");
    let cart_id = summary["lists"][1]["id"].as_str().unwrap();
    let result = inspector()
        .args(["list", "--list-id", cart_id])
        .output()
        .unwrap();
    let rows: Vec<Value> = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["number"], "S1");
    assert!(!inspector().arg("go").output().unwrap().status.success());
}

#[test]
fn live_status_uses_one_status_request_with_the_local_monitor_credential() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let folder = std::env::temp_dir().join(format!("inkue-monitor-{}", std::process::id()));
    let session_file = folder.join("monitor-session.json");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        &session_file,
        json!({"port": port, "token": "monitor"}).to_string(),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut input = BufReader::new(&mut stream);
        let mut line = String::new();
        input.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "POST /v1/command HTTP/1.1");
        let mut length = 0;
        let mut authenticated = false;
        loop {
            line.clear();
            assert!(input.read_line(&mut line).unwrap() > 0);
            if line.trim().is_empty() {
                break;
            }
            let (key, value) = line.trim().split_once(':').unwrap();
            if key.eq_ignore_ascii_case("authorization") {
                authenticated = value.trim() == "Bearer monitor";
            }
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
        assert!(authenticated);
        let mut body = vec![0; length];
        input.read_exact(&mut body).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"command": "live_status"})
        );
        let id = "49300109-925e-4cc6-9946-6185a3a46fb8";
        let response =
            json!({"workspace_id": id, "name": "Live", "active_cue_list_id": id, "lists": []})
                .to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
    });
    let result = Command::new(env!("CARGO_BIN_EXE_inkue-inspect"))
        .arg("--live")
        .arg(&session_file)
        .arg("status")
        .output()
        .unwrap();
    std::fs::remove_dir_all(folder).unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    server.join().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["name"],
        "Live"
    );
    assert!(!inspector().arg("status").output().unwrap().status.success());
}

#[test]
fn mcp_negotiates_and_only_advertises_read_operations() {
    let mut child = inspector()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut send = |request: Value| {
        writeln!(input, "{}", request).unwrap();
        input.flush().unwrap();
    };
    let mut receive = || {
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    send(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}
        }}),
    );
    assert!(receive()["result"]["capabilities"]["tools"].is_object());
    send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let tools = receive()["result"]["tools"].as_array().unwrap().clone();
    let mut names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "draft_audio_sheet",
            "inspect_cue",
            "list_cues",
            "show_summary"
        ]
    );
    for tool in tools {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
    }
    send(
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_cues","arguments":{}}}),
    );
    let rows: Vec<Value> =
        serde_json::from_str(receive()["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(rows.len(), 5);
    send(
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"inspect_cue","arguments":{"id":rows[1]["id"]}}}),
    );
    let cue: Value =
        serde_json::from_str(receive()["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(cue["properties"]["target_cue_ids"][0], rows[0]["id"]);
    send(
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"go","arguments":{}}}),
    );
    let denied = receive();
    assert!(denied.get("error").is_some() || denied["result"]["isError"] == true);
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn handoff_checks_the_saved_show_and_nested_cart_media() {
    let folder = std::env::temp_dir().join(format!("inkue-handoff-{}", std::process::id()));
    let audio = folder.join("sample.wav");
    let workspace = folder.join("show.inkue");
    let reference = folder.join("manifest.json");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(&audio, b"abc").unwrap();
    let data = json!({"workspace":{"name":"Transfer"}, "cue_lists":[{
        "id":"cart","mode":"cart","name":"Hits","cues":[{
            "id":"parent","name":"Group","type":"group","children":[{
                "id":"sound","name":"One shot","type":"audio","file_path":"sample.wav"}]}]}]});
    std::fs::write(&workspace, data.to_string()).unwrap();
    let inspect = |command: &str| {
        let mut process = Command::new(env!("CARGO_BIN_EXE_inkue-inspect"));
        process.arg(&workspace).arg(command);
        if command == "verify" {
            process.arg(&reference);
        }
        process.output().unwrap()
    };
    let output = inspect("manifest");
    assert!(output.status.success());
    let generated: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        generated["media"]["sample.wav"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    std::fs::write(&reference, output.stdout).unwrap();
    assert!(inspect("verify").status.success());
    std::fs::write(&audio, b"def").unwrap();
    let mismatch = inspect("verify");
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("sample.wav"));
    std::fs::write(&audio, b"abc").unwrap();
    std::fs::write(&workspace, serde_json::to_string_pretty(&data).unwrap()).unwrap();
    assert!(!inspect("verify").status.success());
    std::fs::remove_file(&audio).unwrap();
    assert!(!inspect("manifest").status.success());
    std::fs::remove_dir_all(folder).unwrap();
}
