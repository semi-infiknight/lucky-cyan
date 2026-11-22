use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const WORKSPACE: &str = "7de9869c-8311-4fd2-a710-b9117cd15676";
const LIST: &str = "c30960ab-e278-49b3-9b05-153cf1ce301f";
const CUE: &str = "2a6e7f2b-97b0-415a-bb2a-9b605327e72a";
const PARENT: &str = "79826b6a-e640-432b-b3ff-a670bfd24708";
const TOKEN: &str = "test-review-credential";

struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let folder = std::env::temp_dir().join(format!(
            "inkue-review-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&folder).unwrap();
        Self(folder)
    }

    fn write(&self, name: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn credential(&self, port: u16) -> PathBuf {
        self.write(
            "review.json",
            json!({"port": port, "token": TOKEN}).to_string(),
        )
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn inspector(session: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_inkue-inspect"));
    command.arg("--live").arg(session);
    command
}

fn successful_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn lists() -> Value {
    json!({"workspace_id": WORKSPACE, "workspace_name": "Rehearsal", "lists": [{"id": LIST, "name": "Main"}]})
}

fn read_request(stream: &mut TcpStream) -> Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim_end(), "POST /v1/review HTTP/1.1");
    let mut size = None;
    let mut authenticated = false;
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line.trim().is_empty() {
            break;
        }
        let (name, value) = line.trim().split_once(':').unwrap();
        if name.eq_ignore_ascii_case("authorization") {
            assert!(value.trim().strip_prefix("Bearer ") == Some(TOKEN));
            authenticated = true;
        }
        if name.eq_ignore_ascii_case("content-length") {
            size = Some(value.trim().parse().unwrap());
        }
    }
    assert!(authenticated);
    let mut body = vec![0; size.unwrap()];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn serve(files: &Files, expected: Vec<(Value, &str, Value)>) -> (PathBuf, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let session = files.credential(listener.local_addr().unwrap().port());
    let responses: Vec<_> = expected
        .into_iter()
        .map(|(request, status, reply)| (request, status.to_owned(), reply))
        .collect();
    let server = thread::spawn(move || {
        for (request, status, reply) in responses {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, peer)) => {
                        assert!(peer.ip().is_loopback());
                        break stream;
                    }
                    Err(error)
                        if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => {
                        panic!("review client did not send its expected request: {error}")
                    }
                }
            };
            stream.set_nonblocking(false).unwrap();
            assert_eq!(read_request(&mut stream), request);
            let body = reply.to_string();
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (session, server)
}

#[test]
fn review_uses_workspace_ids_and_preserves_note_files_exactly() {
    let files = Files::new();
    let old = "   Standby 🌦\r\n";
    let new = format!("{}\r\n", "é".repeat(999));
    let expected_file = files.write("old.txt", old);
    let note_file = files.write("replacement.txt", &new);
    let listed_cues =
        json!({"cues": [{"id": CUE, "parent_id": PARENT, "number": "1.5", "name": "Rain"}]});
    let mut list_reply = lists();
    list_reply["token"] = json!(TOKEN);
    let get_lists = || (json!({"command": "lists"}), "200 OK", list_reply.clone());
    let (session, server) = serve(
        &files,
        vec![
            get_lists(),
            get_lists(),
            (
                json!({"command": "cues", "workspace_id": WORKSPACE, "list_id": LIST}),
                "200 OK",
                listed_cues.clone(),
            ),
            get_lists(),
            (
                json!({"command": "note", "workspace_id": WORKSPACE, "list_id": LIST, "cue_id": CUE}),
                "200 OK",
                json!({"note": old}),
            ),
            get_lists(),
            (
                json!({"command": "set_note", "workspace_id": WORKSPACE, "list_id": LIST, "cue_id": CUE, "expected_note": old, "note": new}),
                "200 OK",
                json!({"changed": true, "note": new}),
            ),
        ],
    );
    assert_eq!(
        successful_json(inspector(&session).arg("review-lists").output().unwrap()),
        lists()
    );
    assert_eq!(
        successful_json(
            inspector(&session)
                .args(["review-cues", LIST])
                .output()
                .unwrap()
        ),
        listed_cues
    );
    let current = successful_json(
        inspector(&session)
            .args(["review-note", LIST, CUE])
            .output()
            .unwrap(),
    );
    assert_eq!(current, json!({"note": old}));
    let result = successful_json(
        inspector(&session)
            .args(["review-set-note", LIST, CUE, "--expected-file"])
            .arg(expected_file)
            .arg("--note-file")
            .arg(note_file)
            .output()
            .unwrap(),
    );
    assert_eq!(result, json!({"changed": true, "note": new}));
    server.join().unwrap();
}

#[test]
fn review_name_sends_an_exact_compare_and_replace() {
    let files = Files::new();
    let (session, server) = serve(&files, vec![
        (json!({"command": "lists"}), "200 OK", lists()),
        (json!({
            "command": "set_name", "workspace_id": WORKSPACE, "list_id": LIST, "cue_id": CUE,
            "expected_name": "Old label ", "name": "New label",
        }), "200 OK", json!({"changed": true, "name": "New label"})),
    ]);
    let result = successful_json(inspector(&session)
        .args(["review-set-name", LIST, CUE, "--expected", "Old label ", "--name", "New label"])
        .output().unwrap());
    assert_eq!(result, json!({"changed": true, "name": "New label"}));
    server.join().unwrap();
}

#[test]
fn conflicts_are_readable_and_credentials_stay_out_of_output() {
    let files = Files::new();
    let expected_file = files.write("old.txt", "old");
    let note_file = files.write("replacement.txt", "");
    let (session, server) = serve(
        &files,
        vec![
            (json!({"command": "lists"}), "200 OK", lists()),
            (
                json!({"command": "set_note", "workspace_id": WORKSPACE, "list_id": LIST, "cue_id": CUE, "expected_note": "old", "note": ""}),
                "409 Conflict",
                json!({"error": format!("Note changed. Read it again before editing. {TOKEN}")}),
            ),
        ],
    );
    let output = inspector(&session)
        .args(["review-set-note", LIST, CUE, "--from"])
        .arg(expected_file)
        .arg("--to")
        .arg(note_file)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("409") && stderr.contains("Note changed. Read it again"));
    assert!(!stderr.contains(TOKEN));
    server.join().unwrap();
}

#[test]
fn a_revoked_session_cannot_issue_an_addressed_request() {
    let files = Files::new();
    let (session, server) = serve(
        &files,
        vec![(json!({"command": "lists"}), "410 Gone", json!({}))],
    );
    let output = inspector(&session)
        .args(["review-note", LIST, CUE])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("410") && stderr.contains("Start a new review session"));
    assert!(!stderr.contains(TOKEN));
    server.join().unwrap();
}

#[test]
fn review_validates_utf8_and_byte_limits_before_contacting_the_desk() {
    let files = Files::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let session = files.credential(listener.local_addr().unwrap().port());
    let old = files.write("old.txt", "old");
    for (content, diagnostic) in [
        ("é".repeat(1001).into_bytes(), "2,000 UTF-8 bytes"),
        (vec![0xff], "cue note must be UTF-8"),
    ] {
        let new = files.write("replacement.txt", content);
        let result = inspector(&session)
            .args(["review-set-note", LIST, CUE, "--expected-file"])
            .arg(&old)
            .arg("--note-file")
            .arg(new)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8(result.stderr)
            .unwrap()
            .contains(diagnostic));
    }
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    let result = Command::new(env!("CARGO_BIN_EXE_inkue-inspect"))
        .arg(session)
        .arg("review-lists")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8(result.stderr)
        .unwrap()
        .contains("require --live"));
}
