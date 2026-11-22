//! Opt-in, loopback-only control and cue review. Only the local
//! operator's Tauri command can execute an assistant transport request.
mod review_http;

use super::review_cmds::{apply_review, ReviewRequest};
use super::transport_cmds::{go_locked, stop_locked};
use crate::{
    cue::{traits::Cue, types::CueState},
    show::{
        event_loop::{media_elapsed_ms, remaining_ms},
        Workspace,
    },
    state::AppState,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, UdpSocket},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, TryLockError,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, State};
use tiny_http::{Header, Method, Request, Response, Server};
use uuid::Uuid;

use inkue_control::{
    Action, Command, Credentials, LiveCue, LiveList, LiveRun, LiveRunState, LiveStatus,
};

#[derive(Clone, Serialize)]
struct Pending {
    id: Uuid,
    action: Action,
    label: String,
    revision: u64,
    list: Uuid,
    playhead: Option<Uuid>,
}

impl Pending {
    fn matches_workspace(&self, ws: &Workspace) -> bool {
        self.revision == ws.revision
            && self.list == ws.active_cue_list_id
            && self.playhead == ws.active_cue_list().and_then(|list| list.playhead_cue_id)
    }
}

#[derive(Default)]
pub struct AgentDesk {
    session: Option<Session>,
    review: Option<ReviewAccess>,
    performance_locked: bool,
    armed_until: Option<Instant>,
    pending: Option<Pending>,
    last_result: Option<Value>,
}

struct Session {
    id: Uuid,
    file: PathBuf,
    monitor_file: PathBuf,
}

struct ReviewAccess {
    id: Uuid,
    workspace_id: Uuid,
    file: PathBuf,
    open: Arc<AtomicBool>,
}

impl AgentDesk {
    fn is_current_session(&self, id: Uuid) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.id == id)
    }

    fn disconnect_if_current(&mut self, id: Uuid) -> bool {
        if !self.is_current_session(id) {
            return false;
        }
        self.disconnect();
        true
    }

    pub fn disconnect(&mut self) {
        self.revoke_review();
        if let Some(session) = self.session.take() {
            let _ = fs::remove_file(session.file);
            let _ = fs::remove_file(session.monitor_file);
        }
        self.armed_until = None;
        self.pending = None;
        self.last_result = None;
    }

    fn armed(&self) -> bool {
        !self.performance_locked && self.armed_until.is_some_and(|end| end > Instant::now())
    }

    fn set_performance_lock(&mut self, locked: bool) {
        self.performance_locked = locked;
        if locked {
            self.revoke_review();
            self.armed_until = None;
            self.pending = None;
        }
    }

    fn revoke_review(&mut self) {
        if let Some(review) = self.review.take() {
            review.open.store(false, Ordering::Release);
            let _ = fs::remove_file(review.file);
        }
    }

    fn revoke_review_if_current(&mut self, id: Uuid) -> bool {
        if !self.review.as_ref().is_some_and(|review| review.id == id) {
            return false;
        }
        self.revoke_review();
        true
    }

    fn review_authorized(&self, session: Uuid, id: Uuid) -> bool {
        !self.performance_locked
            && self.is_current_session(session)
            && self.review.as_ref().is_some_and(|review| review.id == id)
    }

    fn status(&self) -> Value {
        json!({"session_file":self.session.as_ref().map(|s| &s.file), "armed":self.armed(),
            "monitor_file":self.session.as_ref().map(|s| &s.monitor_file),
            "review_file":self.review.as_ref().map(|review| &review.file),
            "performance_locked":self.performance_locked, "pending":self.pending.as_ref().filter(|_| self.armed()), "last_result":self.last_result})
    }

    fn log(&mut self, ws: &mut Workspace, event: Value) {
        // Logging advances autosave. Only carry a pending approval across our
        // own audit entry if no edit already invalidated it.
        let previous = ws.revision;
        ws.programming_history
            .push(json!({"time":chrono::Utc::now(),"agent":event}));
        ws.mark_modified();
        if let Some(pending) = &mut self.pending {
            if pending.revision == previous {
                pending.revision = ws.revision;
            }
        }
    }
}

#[tauri::command]
pub fn agent_status(state: State<'_, AppState>) -> Result<Value, String> {
    Ok(state.agent.lock().map_err(|e| e.to_string())?.status())
}

#[tauri::command]
pub fn agent_disconnect(state: State<'_, AppState>) -> Result<(), String> {
    state.agent.lock().map_err(|e| e.to_string())?.disconnect();
    Ok(())
}

#[tauri::command]
pub fn agent_review_connect(
    workspace_id: Uuid,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<Value, String> {
    let workspace = state.workspace.try_lock().map_err(|_| "Desk busy, retry the request")?;
    if workspace.instance_id() != workspace_id {
        return Err("The workspace changed. Open Agent access again.".into());
    }
    let mut desk = state.agent.lock().map_err(|error| error.to_string())?;
    if desk.performance_locked {
        return Err("Show Mode locks cue review".into());
    }
    if desk.review.as_ref().is_some_and(|review| review.workspace_id == workspace_id) {
        return Ok(desk.status());
    }
    desk.revoke_review();
    let session = desk.session.as_ref().ok_or("Start Agent access first")?;
    let directory = session.file.parent().ok_or("The session directory is unavailable")?;
    let session_id = session.id;
    let listener = review_http::bind().map_err(|error| error.to_string())?;
    let port = listener.local_addr().map_err(|error| error.to_string())?.port();
    let id = Uuid::new_v4();
    let token = Uuid::new_v4().to_string();
    let file = directory.join(format!("cue-review-{id}.json"));
    write_credentials(
        &file,
        Credentials {
            port,
            osc_port: None,
            token: token.clone(),
        },
    )?;
    let open = Arc::new(AtomicBool::new(true));
    desk.review = Some(ReviewAccess {
        id,
        workspace_id,
        file,
        open: Arc::clone(&open),
    });
    if let Err(error) = std::thread::Builder::new()
        .name("inkue-review-http".into())
        .spawn(move || {
            let state = app_handle.state::<AppState>();
            let authorization = format!("Bearer {token}");
            if let Err(error) = review_http::serve(listener, &open, &authorization, |body| {
                apply_review_body(body, session_id, id, &state, &app_handle)
            }) {
                if state
                    .agent
                    .lock()
                    .is_ok_and(|mut desk| desk.revoke_review_if_current(id))
                {
                    log::warn!("Local Cue review listener stopped: {error}");
                }
            }
        })
    {
        desk.revoke_review();
        return Err(error.to_string());
    }
    Ok(desk.status())
}

#[tauri::command]
pub fn agent_review_disconnect(workspace_id: Uuid, state: State<'_, AppState>) -> Result<Value, String> {
    let workspace = state.workspace.try_lock().map_err(|_| "Desk busy, retry the request")?;
    if workspace.instance_id() != workspace_id {
        return Err("The workspace changed. Open Agent access again.".into());
    }
    let mut desk = state.agent.lock().map_err(|error| error.to_string())?;
    desk.revoke_review();
    Ok(desk.status())
}

#[tauri::command]
pub fn agent_arm(enabled: bool, state: State<'_, AppState>) -> Result<Value, String> {
    let mut desk = state.agent.lock().map_err(|e| e.to_string())?;
    if enabled && desk.performance_locked {
        return Err("Show Mode locks assistant transport requests".into());
    }
    if enabled && desk.session.is_none() {
        return Err("Connect a session first".into());
    }
    desk.pending = None;
    desk.armed_until = enabled.then(|| Instant::now() + Duration::from_secs(60));
    Ok(desk.status())
}

#[tauri::command]
pub fn set_agent_performance_lock(locked: bool, state: State<'_, AppState>) -> Result<(), String> {
    state
        .agent
        .lock()
        .map_err(|e| e.to_string())?
        .set_performance_lock(locked);
    Ok(())
}

#[tauri::command]
pub fn agent_connect(state: State<'_, AppState>, app_handle: AppHandle) -> Result<Value, String> {
    let mut desk = state.agent.lock().map_err(|e| e.to_string())?;
    if desk.session.is_some() {
        return Ok(desk.status());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let server = Server::from_listener(listener, None).map_err(|e| e.to_string())?;
    let osc = UdpSocket::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    osc.set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|e| e.to_string())?;
    let osc_port = osc.local_addr().map_err(|e| e.to_string())?.port();
    let directory = app_handle
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let id = Uuid::new_v4();
    let token = Uuid::new_v4().to_string();
    let file = directory.join(format!("operator-session-{id}.json"));
    let monitor_token = Uuid::new_v4().to_string();
    let monitor_file = directory.join(format!("monitor-session-{id}.json"));
    write_credentials(
        &file,
        Credentials {
            port,
            osc_port: Some(osc_port),
            token: token.clone(),
        },
    )?;
    if let Err(error) = write_credentials(
        &monitor_file,
        Credentials {
            port,
            osc_port: None,
            token: monitor_token.clone(),
        },
    ) {
        let _ = fs::remove_file(&file);
        return Err(error);
    }
    desk.session = Some(Session {
        id,
        file,
        monitor_file,
    });
    let osc_app = app_handle.clone();
    let osc_token = token.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("inkue-agent-osc".into())
        .spawn(move || serve_osc(osc, osc_token, id, osc_app))
    {
        desk.disconnect();
        return Err(e.to_string());
    }
    if let Err(e) = std::thread::Builder::new()
        .name("inkue-agent".into())
        .spawn(move || {
            let auth = format!("Bearer {token}");
            let monitor_auth = format!("Bearer {monitor_token}");
            let state = app_handle.state::<AppState>();
            while session_open(&state, id) {
                match server.recv_timeout(Duration::from_millis(200)) {
                    Ok(Some(request)) => {
                        respond(request, &auth, &monitor_auth, id, &state, &app_handle)
                    }
                    Ok(None) => continue,
                    Err(error) => {
                        listener_failed(&state, id, "HTTP", error);
                        break;
                    }
                }
            }
        })
    {
        desk.disconnect();
        return Err(e.to_string());
    }
    Ok(desk.status())
}

fn write_credentials(file: &std::path::Path, credentials: Credentials) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(file).map_err(|e| e.to_string())?;
    if let Err(e) = writeln!(output, "{}", json!(credentials)) {
        drop(output);
        let _ = fs::remove_file(file);
        return Err(e.to_string());
    }
    Ok(())
}

fn session_open(state: &AppState, id: Uuid) -> bool {
    state
        .agent
        .lock()
        .is_ok_and(|desk| desk.is_current_session(id))
}

fn listener_failed(state: &AppState, id: Uuid, transport: &str, error: std::io::Error) {
    if state
        .agent
        .lock()
        .is_ok_and(|mut desk| desk.disconnect_if_current(id))
    {
        log::warn!("Local Agent {transport} listener stopped: {error}");
    }
}

fn osc_command(packet: &[u8], token: &str) -> Option<Result<Command, String>> {
    use rosc::{OscPacket, OscType};
    // Accept a single message. Bundles and scheduled transport are outside this interface.
    if !packet.starts_with(b"/inkue/agent/command\0") {
        return None;
    }
    let (rest, OscPacket::Message(message)) = rosc::decoder::decode_udp(packet).ok()? else {
        return None;
    };
    let [OscType::String(credential), OscType::String(json)] = message.args.as_slice() else {
        return None;
    };
    if !rest.is_empty() || credential != token || message.addr != "/inkue/agent/command" {
        return None;
    }
    Some(serde_json::from_str(json).map_err(|e| e.to_string()))
}

fn serve_osc(socket: UdpSocket, token: String, session: Uuid, app: AppHandle) {
    let state = app.state::<AppState>();
    let mut buffer = [0u8; 8193];
    while session_open(&state, session) {
        let (size, peer) = match socket.recv_from(&mut buffer) {
            Ok(packet) => packet,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(error) => {
                listener_failed(&state, session, "OSC", error);
                break;
            }
        };
        if size == buffer.len() || !peer.ip().is_loopback() {
            continue;
        }
        let Some(command) = osc_command(&buffer[..size], &token) else {
            continue;
        };
        let value = command
            .and_then(|cmd| dispatch(cmd, session, &state, &app))
            .unwrap_or_else(|e| json!({"error":e}));
        let mut reply = value.to_string();
        if reply.len() > 60_000 {
            reply =
                json!({"error":"Result exceeds one UDP datagram. Use the HTTP or MCP interface."})
                    .to_string();
        }
        let packet = rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/inkue/agent/result".into(),
            args: vec![rosc::OscType::String(reply)],
        });
        if let Ok(bytes) = rosc::encoder::encode(&packet) {
            let _ = socket.send_to(&bytes, peer);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HttpScope {
    Agent,
    Monitor,
}

impl HttpScope {
    fn permits(self, command: &Command) -> bool {
        self == Self::Agent || matches!(command, Command::LiveStatus)
    }
}

fn bearer_header(headers: &[Header]) -> Option<&str> {
    if headers.iter().any(|h| h.field.equiv("Origin")) {
        return None;
    }
    let mut authorization = headers.iter().filter(|h| h.field.equiv("Authorization"));
    let value = authorization.next()?.value.as_str();
    if authorization.next().is_some() {
        return None;
    }
    Some(value)
}

fn http_scope(headers: &[Header], auth: &str, monitor_auth: &str) -> Option<HttpScope> {
    let value = bearer_header(headers)?;
    if value == auth {
        Some(HttpScope::Agent)
    } else if value == monitor_auth {
        Some(HttpScope::Monitor)
    } else {
        None
    }
}

fn respond(
    mut request: Request,
    auth: &str,
    monitor_auth: &str,
    session: Uuid,
    state: &AppState,
    app: &AppHandle,
) {
    let scope = http_scope(request.headers(), auth, monitor_auth);
    let result = if scope.is_none() {
        Err((403, "Access denied".to_string()))
    } else if request.method() != &Method::Post || request.url() != "/v1/command" {
        Err((404, "Unknown operation".into()))
    } else if !matches!(request.body_length(), Some(1..=8192)) {
        Err((413, "Expected a JSON command of at most 8192 bytes".into()))
    } else {
        serde_json::from_reader::<_, Command>(request.as_reader().take(8193))
            .map_err(|e| (400, e.to_string()))
            .and_then(|cmd| {
                if !scope.is_some_and(|scope| scope.permits(&cmd)) {
                    return Err((403, "The monitor credential only grants live status".into()));
                }
                dispatch(cmd, session, state, app).map_err(|e| {
                    let status = if e == "Session closed" { 410 } else { 409 };
                    (status, e)
                })
            })
    };
    respond_json(request, result);
}

fn apply_review_body(
    body: &[u8],
    session: Uuid,
    review_id: Uuid,
    state: &AppState,
    app: &AppHandle,
) -> Result<Value, (u16, String)> {
    let command = serde_json::from_slice::<ReviewRequest>(body)
        .map_err(|_| (400, "Invalid cue review command".into()))?;
    let mut workspace = state
        .workspace
        .try_lock()
        .map_err(|_| (409, "Desk busy, retry the request".into()))?;
    let mut desk = state
        .agent
        .try_lock()
        .map_err(|_| (409, "Desk busy, retry the request".into()))?;
    if !desk.review_authorized(session, review_id) {
        return Err((410, "Session closed".into()));
    }
    if !desk
        .review
        .as_ref()
        .is_some_and(|review| review.workspace_id == workspace.instance_id())
    {
        desk.revoke_review();
        return Err((410, "The workspace changed. Reconnect from the desk.".into()));
    }
    let mut history = command.is_write().then(|| state.undo_stack.try_lock())
        .transpose().map_err(|_| (409, "Desk busy, retry the request".into()))?;
    let (response, changed) = apply_review(&mut workspace, command, history.as_deref_mut())
        .map_err(|error| (409, error))?;
    drop(history);
    drop(desk);
    drop(workspace);
    if changed {
        let _ = app.emit("workspace-modified", ());
    }
    Ok(response)
}

fn respond_json(request: Request, result: Result<Value, (u16, String)>) {
    let (code, value) = match result {
        Ok(value) => (200, value),
        Err((code, message)) => (code, json!({"error":message})),
    };
    let header = Header::from_bytes("Content-Type", "application/json").expect("static header");
    let no_cache = Header::from_bytes("Cache-Control", "no-store").expect("static header");
    let _ = request.respond(
        Response::from_string(value.to_string())
            .with_header(header)
            .with_header(no_cache)
            .with_status_code(code),
    );
}

fn list_rows(cues: &[Box<dyn Cue>], list: Uuid, parent: Option<Uuid>, rows: &mut Vec<Value>) {
    for cue in cues {
        rows.push(json!({"list_id":list,"parent_id":parent,"id":cue.id(),"number":cue.number(),
            "name":cue.name(),"type":cue.cue_type(),"is_disabled":cue.is_disabled(),"state":cue.state()}));
        if let Some(children) = cue.child_cues() {
            list_rows(children, list, Some(cue.id()), rows);
        }
    }
}

fn monitor_cue(cue: &dyn Cue) -> LiveCue {
    LiveCue {
        id: cue.id(),
        number: cue.number().map(str::to_owned),
        name: cue.name().to_owned(),
        kind: cue.cue_type().to_string(),
        is_disabled: cue.is_disabled(),
    }
}

fn monitor_runs(
    cues: &[Box<dyn Cue>],
    parent_id: Option<Uuid>,
    runs: &mut Vec<LiveRun>,
    timing: &impl Fn(&dyn Cue) -> (u64, Option<u64>),
) {
    for cue in cues {
        let state = match cue.state() {
            CueState::Running => Some(LiveRunState::Running),
            CueState::Paused => Some(LiveRunState::Paused),
            _ => None,
        };
        if let Some(state) = state {
            let (elapsed_ms, remaining_ms) = timing(cue.as_ref());
            runs.push(LiveRun {
                cue: monitor_cue(cue.as_ref()),
                parent_id,
                state,
                elapsed_ms,
                remaining_ms,
            });
        }
        if let Some(children) = cue.child_cues() {
            monitor_runs(children, Some(cue.id()), runs, timing);
        }
    }
}

fn monitor_status(ws: &Workspace, timing: impl Fn(&dyn Cue) -> (u64, Option<u64>)) -> LiveStatus {
    LiveStatus {
        workspace_id: ws.instance_id,
        name: ws.metadata.name.clone(),
        active_cue_list_id: ws.active_cue_list_id,
        lists: ws
            .cue_lists
            .iter()
            .map(|list| {
                let mut active = Vec::new();
                monitor_runs(&list.cues, None, &mut active, &timing);
                LiveList {
                    id: list.id,
                    name: list.name.clone(),
                    standby: list.playhead_cue().map(monitor_cue),
                    active,
                }
            })
            .collect(),
    }
}

/// Show metadata for the local desk. The WebView already has the sampled
/// playback clocks, so this read never needs the audio or video engine.
#[tauri::command]
pub fn get_cue_monitor_snapshot(
    workspace_id: Uuid,
    state: State<'_, AppState>,
) -> Result<Option<LiveStatus>, String> {
    local_monitor_snapshot(&state.workspace, workspace_id)
}

fn local_monitor_snapshot(
    workspace: &Mutex<Workspace>,
    workspace_id: Uuid,
) -> Result<Option<LiveStatus>, String> {
    let ws = match workspace.try_lock() {
        Ok(ws) => ws,
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Poisoned(error)) => return Err(error.to_string()),
    };
    if ws.instance_id != workspace_id {
        return Err("Workspace changed.".into());
    }
    Ok(Some(monitor_status(&ws, |_| (0, None))))
}

fn dispatch(
    command: Command,
    session: Uuid,
    state: &AppState,
    app: &AppHandle,
) -> Result<Value, String> {
    // No waiting for the live desk to yield its show lock.
    let mut ws = state
        .workspace
        .try_lock()
        .map_err(|_| "Desk busy, retry the request")?;
    let mut desk = state.agent.lock().map_err(|e| e.to_string())?;
    if !desk.is_current_session(session) {
        return Err("Session closed".into());
    }
    let (operation, result) = match command {
        Command::LiveStatus => {
            // Status can be polled during a show without creating audit entries,
            // changing its revision, or refreshing a pending approval.
            let status = monitor_status(&ws, |cue| {
                let elapsed = media_elapsed_ms(cue, &state.audio_engine, &state.output_engine);
                (elapsed, remaining_ms(cue, elapsed, &state.audio_engine))
            });
            return serde_json::to_value(status).map_err(|error| error.to_string());
        }
        Command::Summary => (
            "summary",
            Ok(json!({"source":"live_workspace","name":ws.metadata.name,
            "active_cue_list_id":ws.active_cue_list_id,"lists":ws.cue_lists.iter().map(|list|
                json!({"id":list.id,"name":list.name,"mode":list.mode,"playhead_cue_id":list.playhead_cue_id})).collect::<Vec<_>>(),
            "agent": {"armed":desk.armed(),"last_result":desk.last_result}})),
        ),
        Command::List { list_id } => {
            let mut rows = vec![];
            let mut found = false;
            for list in ws
                .cue_lists
                .iter()
                .filter(|l| list_id.is_none_or(|id| id == l.id))
            {
                found = true;
                list_rows(&list.cues, list.id, None, &mut rows);
            }
            (
                "list",
                if found {
                    Ok(json!(rows))
                } else {
                    Err("Unknown cue list".into())
                },
            )
        }
        Command::Cue { id } => (
            "cue",
            ws.cue_lists
                .iter()
                .find_map(|list| list.get_recursive(&id))
                .map(|cue| cue.serialize())
                .ok_or_else(|| "Unknown cue".to_string()),
        ),
        Command::Transport {
            action,
            request_approval,
        } => {
            let result = prepare(&ws, &mut desk, action, request_approval);
            desk.log(&mut ws, json!({"operation":"transport","action":action,
                "requested_approval":request_approval,"outcome":result.as_ref().map(|v| &v["status"]).map_err(String::as_str)}));
            let _ = app.emit("workspace-modified", ());
            return result;
        }
    };
    desk.log(&mut ws, json!({"operation":operation,"ok":result.is_ok()}));
    let _ = app.emit("workspace-modified", ());
    result
}

fn prepare(
    ws: &Workspace,
    desk: &mut AgentDesk,
    action: Action,
    request: bool,
) -> Result<Value, String> {
    let list = ws.active_cue_list().ok_or("No active list")?;
    let label = match action {
        Action::Go => {
            let cue = list.playhead_cue().ok_or("No cue at the playhead")?;
            if cue.is_disabled() {
                return Err("The playhead cue is disarmed".into());
            }
            format!("GO {} {}", cue.number().unwrap_or(""), cue.name())
        }
        Action::Stop => "Stop all running cues".into(),
        Action::Panic => "Hard stop all outputs".into(),
    };
    if !request {
        return Ok(json!({"status":"preview","label":label,"armed":desk.armed()}));
    }
    if desk.performance_locked {
        return Err("Show Mode locks assistant transport requests".into());
    }
    if !desk.armed() {
        return Err("Live access is disarmed at the desk".into());
    }
    if desk.pending.is_some() {
        return Err("Another action is awaiting review".into());
    }
    let pending = Pending {
        id: Uuid::new_v4(),
        action,
        label,
        revision: ws.revision,
        list: list.id,
        playhead: list.playhead_cue_id,
    };
    let result = json!({"status":"awaiting_operator","request":pending});
    desk.pending = Some(pending);
    Ok(result)
}

#[tauri::command]
pub fn agent_decide(
    id: Uuid,
    approve: bool,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<Value, String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut desk = state.agent.lock().map_err(|e| e.to_string())?;
    if !desk.pending.as_ref().is_some_and(|p| p.id == id) {
        return Err("Request is no longer pending".into());
    }
    let pending = desk.pending.take().expect("checked above");
    let armed = desk.armed();
    desk.armed_until = None;
    let unchanged = pending.matches_workspace(&ws);
    let outcome = if !approve {
        Ok("declined")
    } else if !armed || !unchanged {
        Err("Request expired or the show changed".to_string())
    } else {
        match pending.action {
            Action::Go => go_locked(&state, &app_handle, &mut ws).map(|fired| {
                if fired {
                    "executed"
                } else {
                    "suppressed"
                }
            }),
            action => stop_locked(&state, &app_handle, &mut ws, action == Action::Panic)
                .map(|_| "executed"),
        }
    };
    let record = json!({"id":id,"action":pending.action,"label":pending.label,
        "outcome":outcome.as_ref().map_or_else(|e| e.as_str(), |s| *s)});
    desk.log(&mut ws, record.clone());
    desk.last_result = Some(record);
    let _ = app_handle.emit("workspace-modified", ());
    outcome.map(|_| desk.status())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::memo_cue::MemoCue;

    #[test]
    fn show_mode_revokes_note_review_without_rotating_agent_and_monitor() {
        let folder = tempfile::tempdir().unwrap();
        let session = Uuid::new_v4();
        let review_file = folder.path().join("review.json");
        let agent_file = folder.path().join("agent.json");
        let monitor_file = folder.path().join("monitor.json");
        let review_id = Uuid::new_v4();
        let review_open = Arc::new(AtomicBool::new(true));
        for path in [&review_file, &agent_file, &monitor_file] {
            fs::write(path, "{}").unwrap();
        }
        let mut desk = AgentDesk {
            session: Some(Session {
                id: session,
                file: agent_file.clone(),
                monitor_file: monitor_file.clone(),
            }),
            review: Some(ReviewAccess {
                id: review_id,
                workspace_id: Uuid::new_v4(),
                file: review_file.clone(),
                open: Arc::clone(&review_open),
            }),
            ..Default::default()
        };
        assert!(desk.review_authorized(session, review_id));
        assert!(!desk.review_authorized(Uuid::new_v4(), review_id));
        let stale_review = Uuid::new_v4();
        assert!(!desk.review_authorized(session, stale_review));
        assert!(!desk.revoke_review_if_current(stale_review));
        assert!(review_open.load(Ordering::Acquire));
        assert!(http_scope(&[Header::from_bytes("Authorization", "Bearer review").unwrap()],
            "Bearer agent", "Bearer monitor").is_none());
        assert!(bearer_header(&[Header::from_bytes("Authorization", "Bearer review").unwrap(),
            Header::from_bytes("Origin", "http://localhost").unwrap()]).is_none());
        desk.set_performance_lock(true);
        assert!(desk.status()["review_file"].is_null());
        assert!(!review_file.exists() && agent_file.exists() && monitor_file.exists());
        assert!(!review_open.load(Ordering::Acquire));
        desk.set_performance_lock(false);
        assert!(!desk.review_authorized(session, review_id));
    }

    #[test]
    fn failed_listener_only_revokes_the_session_it_served() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("current-agent.json");
        let monitor_file = folder.path().join("current-monitor.json");
        fs::write(&file, "{}").unwrap();
        fs::write(&monitor_file, "{}").unwrap();
        let current = Uuid::new_v4();
        let mut desk = AgentDesk {
            session: Some(Session {
                id: current,
                file: file.clone(),
                monitor_file: monitor_file.clone(),
            }),
            armed_until: Some(Instant::now() + Duration::from_secs(60)),
            ..Default::default()
        };
        assert!(!desk.disconnect_if_current(Uuid::new_v4()));
        assert!(desk.is_current_session(current) && desk.armed());
        assert!(file.is_file() && monitor_file.is_file());
        assert!(desk.disconnect_if_current(current));
        assert!(!desk.is_current_session(current) && !desk.armed());
        assert!(!file.exists() && !monitor_file.exists());
    }

    #[test]
    fn monitor_credential_cannot_inspect_cue_properties_or_request_transport() {
        let bearer = Header::from_bytes("Authorization", "Bearer monitor").unwrap();
        let headers = [bearer.clone()];
        let scope = http_scope(&headers, "Bearer agent", "Bearer monitor").unwrap();
        assert!(scope.permits(&Command::LiveStatus));
        assert!(!scope.permits(&Command::Cue { id: Uuid::new_v4() }));
        assert!(!scope.permits(&Command::List { list_id: None }));
        assert!(!scope.permits(&Command::Transport {
            action: Action::Go,
            request_approval: true
        }));
        let origin = Header::from_bytes("Origin", "http://127.0.0.1").unwrap();
        assert!(http_scope(&[bearer.clone(), origin], "Bearer agent", "Bearer monitor").is_none());
        assert!(http_scope(&[bearer.clone(), bearer], "Bearer agent", "Bearer monitor").is_none());
    }

    #[test]
    fn monitor_reads_the_standby_without_an_edit_or_history_entry() {
        let mut ws = Workspace::new("Rehearsal");
        let list = ws.active_cue_list_mut().unwrap();
        let mut memo = MemoCue::new();
        memo.set_name("Crossing".into());
        let id = memo.id();
        list.push(Box::new(memo));
        list.set_playhead(Some(id)).unwrap();
        let (revision, history) = (ws.revision, ws.programming_history.len());
        let status = monitor_status(&ws, |_| panic!("standby has no active timing"));
        assert_eq!(status.workspace_id, ws.instance_id);
        assert_eq!(status.active_cue_list_id, ws.active_cue_list_id);
        let list = &status.lists[0];
        assert_eq!(list.standby.as_ref().unwrap().id, id);
        assert_eq!(list.standby.as_ref().unwrap().name, "Crossing");
        assert!(list.active.is_empty());
        assert_eq!(
            (ws.revision, ws.programming_history.len()),
            (revision, history)
        );
    }

    #[test]
    fn local_monitor_skips_busy_reads_and_rejects_a_replaced_show() {
        let workspace = Mutex::new(Workspace::new("First rehearsal"));
        let locked = workspace.lock().unwrap();
        let owner = locked.instance_id;
        assert!(local_monitor_snapshot(&workspace, owner).unwrap().is_none());
        drop(locked);
        let first = local_monitor_snapshot(&workspace, owner).unwrap().unwrap();
        assert_eq!(first.workspace_id, owner);
        assert_eq!(first.name, "First rehearsal");

        *workspace.lock().unwrap() = Workspace::new("Second rehearsal");
        assert!(matches!(
            local_monitor_snapshot(&workspace, owner),
            Err(error) if error == "Workspace changed."
        ));
    }

    #[test]
    fn entering_show_mode_cancels_pending_requests_and_leaves_inspection_available() {
        let mut ws = Workspace::new("Rehearsal");
        let list = ws.active_cue_list_mut().unwrap();
        let memo = MemoCue::new();
        let id = crate::cue::traits::Cue::id(&memo);
        list.push(Box::new(memo));
        list.set_playhead(Some(id)).unwrap();
        let mut desk = AgentDesk {
            armed_until: Some(Instant::now() + Duration::from_secs(60)),
            ..Default::default()
        };
        assert!(prepare(&ws, &mut desk, Action::Go, true).is_ok());
        assert!(desk.pending.is_some());
        desk.set_performance_lock(true);
        assert!(!desk.armed());
        assert!(desk.pending.is_none());
        assert!(prepare(&ws, &mut desk, Action::Go, true)
            .unwrap_err()
            .contains("Show Mode"));
        assert_eq!(
            prepare(&ws, &mut desk, Action::Go, false).unwrap()["status"],
            "preview"
        );
        desk.disconnect();
        assert!(desk.performance_locked);
        desk.set_performance_lock(false);
        assert!(!desk.armed());
    }

    #[test]
    fn osc_uses_the_same_commands_and_requires_the_session_credential() {
        let packet = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/inkue/agent/command".into(),
            args: vec![
                rosc::OscType::String("session".into()),
                rosc::OscType::String(r#"{"command":"transport","action":"go"}"#.into()),
            ],
        }))
        .unwrap();
        assert!(osc_command(&packet, "other").is_none());
        assert!(matches!(
            osc_command(&packet, "session"),
            Some(Ok(Command::Transport {
                action: Action::Go,
                request_approval: false
            }))
        ));
    }

    #[test]
    fn preview_never_queues_and_requests_need_a_fresh_local_arm() {
        let ws = Workspace::new("Live");
        let mut desk = AgentDesk::default();
        assert_eq!(
            prepare(&ws, &mut desk, Action::Stop, false).unwrap()["status"],
            "preview"
        );
        assert!(desk.pending.is_none());
        assert!(prepare(&ws, &mut desk, Action::Panic, true).is_err());
        desk.armed_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(prepare(&ws, &mut desk, Action::Stop, true).is_err());
        desk.armed_until = Some(Instant::now() + Duration::from_secs(60));
        assert!(prepare(&ws, &mut desk, Action::Stop, true).is_ok());
        assert!(prepare(&ws, &mut desk, Action::Panic, true).is_err());
        desk.disconnect();
        assert!(!desk.armed() && desk.pending.is_none());
    }

    #[test]
    fn approval_is_bound_to_revision_and_playhead() {
        let mut ws = Workspace::new("Rehearsal");
        ws.active_cue_list_mut()
            .unwrap()
            .push(Box::new(MemoCue::new()));
        let mut desk = AgentDesk {
            armed_until: Some(Instant::now() + Duration::from_secs(60)),
            ..Default::default()
        };
        prepare(&ws, &mut desk, Action::Go, true).unwrap();
        let first = ws.active_cue_list().unwrap().playhead_cue_id;
        desk.log(&mut ws, json!({"operation":"summary"}));
        assert!(desk.pending.as_ref().unwrap().matches_workspace(&ws));
        ws.active_cue_list_mut().unwrap().playhead_cue_id = None;
        assert!(!desk.pending.as_ref().unwrap().matches_workspace(&ws));
        ws.active_cue_list_mut().unwrap().playhead_cue_id = first;
        ws.mark_modified();
        desk.log(&mut ws, json!({"operation":"summary"}));
        assert!(!desk.pending.as_ref().unwrap().matches_workspace(&ws));
    }
}
