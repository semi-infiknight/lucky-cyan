//! Show-control receiver for OSC and optional plain text UDP.
//!
//! Architecture:
//! - One background thread per socket.
//! - `recv_from` with a 100 ms read timeout so config changes take effect quickly.
//! - Native transport handles workspace commands and addressed cue actions.
//! - A few binary OSC queries reply directly to an explicitly allowlisted peer.

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use tauri::{Emitter, Manager};
use uuid::Uuid;

use crate::cue::{traits::Cue, types::CueId};
use crate::commands::transport_cmds::{
    address_network_cue_pattern, address_numbered_network_cue, control_network_list_pause,
    control_network_workspace, NetworkCueAction, NetworkPauseAction, NetworkWorkspaceAction,
};
use crate::preferences::OscReceiveConfig;
use crate::show::Workspace;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Dedup cache — prevents duplicate UDP packets (Windows loopback quirk, some
// OSC controllers that send each message twice) from being acted upon twice.
// ---------------------------------------------------------------------------

/// Holds a short rolling window of recently-seen packets.
struct DedupCache {
    /// (received_at, fingerprint)
    entries: VecDeque<(Instant, u64)>,
    /// How long to keep an entry before considering it stale.
    window: Duration,
}

impl DedupCache {
    fn new(window: Duration) -> Self {
        Self { entries: VecDeque::with_capacity(32), window }
    }

    /// Returns `true` if this fingerprint was already seen within the window
    /// (i.e. this is a duplicate).  Otherwise records it and returns `false`.
    fn is_duplicate(&mut self, fp: u64) -> bool {
        let now = Instant::now();
        // Purge stale entries.
        while self.entries.front().is_some_and(|(t, _)| now.duration_since(*t) > self.window) {
            self.entries.pop_front();
        }
        if self.entries.iter().any(|(_, f)| *f == fp) {
            return true;
        }
        self.entries.push_back((now, fp));
        false
    }
}

/// Cheap fingerprint for a received OSC message.
fn packet_fingerprint(buf: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    buf.hash(&mut h);
    h.finish()
}

/// Configure binary OSC and plain text independently while sharing command dispatch.
pub struct OscServer {
    osc: Listener,
    plain_text: Listener,
}

struct Listener {
    commands: Sender<ListenerCommand>,
    status: Arc<Mutex<OscListenerStatus>>,
}

#[derive(Clone, Copy)]
enum Protocol {
    Osc,
    PlainText,
}

impl Protocol {
    fn settings(self, config: &OscReceiveConfig) -> ListenerConfig {
        let (enabled, port) = match self {
            Self::Osc => (config.enabled, config.port),
            Self::PlainText => (config.plain_text_enabled, config.plain_text_port),
        };
        ListenerConfig { enabled, port, allowed_ips: config.allowed_ips.clone() }
    }

    fn allows(self, allowlist: &[String], sender: IpAddr) -> bool {
        (matches!(self, Self::Osc) || !allowlist.is_empty())
            && is_allowed(allowlist, &sender.to_string())
    }

    fn decode(self, data: &[u8]) -> Option<rosc::OscPacket> {
        match self {
            Self::Osc => rosc::decoder::decode_udp(data).ok().map(|(_, packet)| packet),
            Self::PlainText => parse_text_message(data).map(rosc::OscPacket::Message),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Osc => "OSC",
            Self::PlainText => "Plain text UDP",
        }
    }

    fn status_event(self) -> &'static str {
        match self {
            Self::Osc => "osc-listener-status",
            Self::PlainText => "plain-text-listener-status",
        }
    }
}

struct ListenerConfig {
    enabled: bool,
    port: u16,
    allowed_ips: Vec<String>,
}

/// Live state of one receive socket.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OscListenerStatus {
    /// Waiting for the first socket bind.
    Binding,
    /// The receive configuration is disabled.
    Disabled,
    /// The actual socket address, including the port assigned by the OS.
    Listening { address: SocketAddr },
    /// The configured socket address and operating system error.
    BindFailed { address: SocketAddr, error: String },
}

enum ListenerCommand {
    Configure(ListenerConfig),
    Retry,
    Stop,
}

impl OscServer {
    /// Start both workers with their independently enabled UDP endpoints.
    pub fn start(config: OscReceiveConfig, app_handle: tauri::AppHandle) -> Self {
        let spawn = |protocol: Protocol| {
            let status_handle = app_handle.clone();
            let packet_handle = app_handle.clone();
            let query_handle = app_handle.clone();
            Listener::spawn(
                protocol,
                protocol.settings(&config),
                super::net_interface::bind_ip,
                move |status| { let _ = status_handle.emit(protocol.status_event(), status); },
                move |packet, explicitly_allowed| handle_packet(packet, explicitly_allowed, &packet_handle),
                move |query| handle_query(query, &query_handle),
            )
        };
        Self { osc: spawn(Protocol::Osc), plain_text: spawn(Protocol::PlainText) }
    }

    pub fn reconfigure(&self, config: OscReceiveConfig) {
        self.osc.reconfigure(Protocol::Osc.settings(&config));
        self.plain_text.reconfigure(Protocol::PlainText.settings(&config));
    }

    fn listener(&self, plain_text: bool) -> &Listener {
        if plain_text { &self.plain_text } else { &self.osc }
    }

    pub fn status(&self, plain_text: bool) -> Result<OscListenerStatus, String> {
        self.listener(plain_text).status()
    }

    pub fn retry(&self, plain_text: bool) -> Result<(), String> {
        self.listener(plain_text).retry()
    }

    /// Shut down both workers.
    pub fn stop(&self) {
        self.osc.stop();
        self.plain_text.stop();
    }
}

impl Listener {
    fn spawn(
        protocol: Protocol,
        config: ListenerConfig,
        bind_ip: impl Fn() -> IpAddr + Send + 'static,
        mut on_status: impl FnMut(&OscListenerStatus) + Send + 'static,
        on_packet: impl FnMut(&rosc::OscPacket, bool) + Send + 'static,
        on_query: impl FnMut(CueQuery<'_>) -> Option<Vec<u8>> + Send + 'static,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let initial = if config.enabled { OscListenerStatus::Binding } else { OscListenerStatus::Disabled };
        let status = Arc::new(Mutex::new(initial));
        let listener_status = status.clone();

        std::thread::Builder::new()
            .name(format!("inkue-{}-receiver", protocol.status_event()))
            .spawn(move || server_loop(protocol, config, rx, bind_ip, |status| {
                if let Ok(mut shared) = listener_status.lock() {
                    *shared = status.clone();
                }
                on_status(&status);
            }, on_packet, on_query))
            .expect("Failed to spawn UDP listener thread");

        Self { commands: tx, status }
    }

    /// Apply a new configuration without restarting the app.  The listener
    /// thread picks up the change within 100 ms.
    fn reconfigure(&self, config: ListenerConfig) {
        let _ = self.commands.send(ListenerCommand::Configure(config));
    }

    /// Return the state reported by the receive socket.
    fn status(&self) -> Result<OscListenerStatus, String> {
        self.status.lock().map(|status| status.clone()).map_err(|error| error.to_string())
    }

    /// Re-attempt a failed bind using the listener's current configuration.
    /// A disabled receiver stays disabled and a healthy socket stays open.
    fn retry(&self) -> Result<(), String> {
        self.commands.send(ListenerCommand::Retry)
            .map_err(|_| "The UDP listener has stopped.".to_string())
    }

    /// Gracefully shut down the listener thread.
    fn stop(&self) {
        let _ = self.commands.send(ListenerCommand::Stop);
    }
}

// ---------------------------------------------------------------------------
// Internal loop
// ---------------------------------------------------------------------------

fn server_loop(
    protocol: Protocol,
    mut config: ListenerConfig,
    commands: Receiver<ListenerCommand>,
    bind_ip: impl Fn() -> IpAddr,
    mut on_status: impl FnMut(OscListenerStatus),
    mut on_packet: impl FnMut(&rosc::OscPacket, bool),
    mut on_query: impl FnMut(CueQuery<'_>) -> Option<Vec<u8>>,
) {
    let label = protocol.label();
    loop {
        if !config.enabled {
            on_status(OscListenerStatus::Disabled);
            loop {
                match commands.recv() {
                    Ok(ListenerCommand::Configure(new)) => { config = new; break; }
                    Ok(ListenerCommand::Retry) => {}
                    Ok(ListenerCommand::Stop) | Err(_) => return,
                }
            }
            continue;
        }

        let addr = SocketAddr::new(bind_ip(), config.port);
        let bind = UdpSocket::bind(addr).and_then(|socket| {
            socket.set_read_timeout(Some(Duration::from_millis(100)))?;
            if matches!(protocol, Protocol::Osc) {
                socket.set_write_timeout(Some(Duration::from_millis(10)))?;
            }
            Ok((socket.local_addr()?, socket))
        });
        let (address, socket) = match bind {
            Ok(bound) => bound,
            Err(e) => {
                log::error!("{label}: failed to bind {addr}: {e}");
                on_status(OscListenerStatus::BindFailed { address: addr, error: e.to_string() });
                match commands.recv_timeout(Duration::from_secs(5)) {
                    Ok(ListenerCommand::Configure(new)) => config = new,
                    Ok(ListenerCommand::Retry) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Ok(ListenerCommand::Stop) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                }
                continue;
            }
        };

        on_status(OscListenerStatus::Listening { address });
        log::info!("{label} server listening on {address}");
        let mut buf = [0u8; 4096];
        // 50 ms window catches Windows loopback duplicates and OSC controllers
        // that send each message twice at the same millisecond.
        let mut dedup = DedupCache::new(Duration::from_millis(50));

        loop {
            // Check for config changes before blocking.
            match commands.try_recv() {
                Ok(ListenerCommand::Configure(new)) => {
                    config = new;
                    // Allowlist and unrelated preference changes can take effect
                    // without dropping a healthy socket.
                    if !config.enabled || SocketAddr::new(bind_ip(), config.port) != addr {
                        break;
                    }
                }
                Ok(ListenerCommand::Stop) | Err(crossbeam_channel::TryRecvError::Disconnected) => return,
                Ok(ListenerCommand::Retry) | Err(crossbeam_channel::TryRecvError::Empty) => {}
            }

            match socket.recv_from(&mut buf) {
                Ok((n, src)) => {
                    if !protocol.allows(&config.allowed_ips, src.ip()) {
                        log::debug!("{label}: ignoring packet from non-allowlisted {src}");
                        continue;
                    }
                    if !crate::engine::overrides::OVERRIDES.network_external_input() && !src.ip().is_loopback() {
                        log::debug!("{label}: dropping external packet while the network input override is off");
                        continue;
                    }
                    let data = &buf[..n];
                    let Some(packet) = protocol.decode(data) else {
                        log::debug!("{label}: ignored malformed or unsupported packet");
                        continue;
                    };
                    if let rosc::OscPacket::Message(message) = &packet {
                        if let Some(query) = CueQuery::parse(message) {
                            if matches!(protocol, Protocol::Osc)
                                && !config.allowed_ips.is_empty()
                                && query.is_single_datagram(data)
                            {
                                if let Some(reply) = on_query(query) {
                                    let _ = socket.send_to(&reply, src);
                                }
                            }
                            // Query results are unicast and can change between identical requests.
                            continue;
                        }
                    }
                    let fp = packet_fingerprint(data);
                    if dedup.is_duplicate(fp) {
                        log::debug!("{label}: dropped duplicate packet from {src}");
                        continue;
                    }
                    on_packet(&packet, !config.allowed_ips.is_empty());
                }
                Err(e) if is_timeout(&e) => {}
                Err(e) => log::warn!("{label}: recv error: {e}"),
            }
        }
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
}

fn is_allowed(allowed_ips: &[String], src_ip: &str) -> bool {
    allowed_ips.is_empty() || allowed_ips.iter().any(|ip| ip == src_ip)
}

/// One UTF-8 command per datagram. Produce the same message the OSC dispatcher
/// would receive from a binary packet, including its addressed-cue guard.
fn parse_text_message(data: &[u8]) -> Option<rosc::OscMessage> {
    if data.len() > 1024 {
        return None;
    }
    let line = std::str::from_utf8(data).ok()?.trim_matches([' ', '\t', '\r', '\n']);
    if line.chars().any(|character| {
        (character.is_control() && character != '\t')
            || (character.is_whitespace() && !character.is_ascii_whitespace())
    }) {
        return None;
    }
    let mut tokens = line.split_ascii_whitespace();
    let raw = tokens.next()?;
    let address = match raw {
        "/go" => "/inkue/go".to_owned(),
        "/stop" => "/inkue/stop".to_owned(),
        raw => raw.to_owned(),
    };
    let seek = if let Some(cue) = address.strip_prefix("/inkue/cue/") {
        let (number, action) = cue.split_once('/')?;
        if !literal_cue_number(number) {
            return None;
        }
        match action {
            "go" | "start" | "select" | "stop" | "pause" | "resume" => None,
            "seek" | "seek/relative" | "seek/percent" => Some(action),
            _ => return None,
        }
    } else {
        None
    };
    let args = if let Some(action) = seek {
        let value: f64 = tokens.next()?.parse().ok()?;
        if !(value * 1000.0).is_finite() || (action == "seek/percent" && !(0.0..=1.0).contains(&value)) {
            return None;
        }
        vec![rosc::OscType::Double(value)]
    } else {
        let rest: Vec<&str> = tokens.collect();
        match rest.as_slice() {
            [] => Vec::new(),
            // One argument for QLab cue property writes (`/cue/5/preWait 3`,
            // `/cue/5/qName LX`); numeric when it parses, string otherwise.
            [one] if address.starts_with("/cue/") || address.starts_with("/cue_id/") => {
                match one.parse::<f64>() {
                    Ok(value) if (value * 1000.0).is_finite() => vec![rosc::OscType::Double(value)],
                    _ => vec![rosc::OscType::String((*one).to_owned())],
                }
            }
            _ => return None,
        }
    };
    if matches!(resolve_action(&address, &args), OscAction::Unmatched) {
        return None;
    }
    Some(rosc::OscMessage { addr: address, args })
}

// ---------------------------------------------------------------------------
// Packet dispatch
// ---------------------------------------------------------------------------

fn handle_packet(packet: &rosc::OscPacket, explicitly_allowed: bool, app_handle: &tauri::AppHandle) {
    match packet {
        rosc::OscPacket::Message(msg) => handle_message(msg, explicitly_allowed, app_handle),
        rosc::OscPacket::Bundle(bundle) => {
            for p in &bundle.content {
                handle_packet(p, explicitly_allowed, app_handle);
            }
        }
    }
}

/// What an incoming OSC message resolves to. Pure classification — the
/// single source of truth for both dispatch and the monitor's matched flag.
enum OscAction<'a> {
    /// Dispatch to the frontend as an `osc-command` payload.
    Command(serde_json::Value),
    /// Resolve the target and run it under the workspace lock.
    AddressedCue { workspace: Option<WorkspaceSelector<'a>>, number: &'a str, action: NetworkCueAction },
    /// QLab wildcard cue address (`/cue/*/stop`, `/cue/*1/reset`): applies to
    /// every matching top-level cue. Transport verbs only; writes allowed.
    AddressedCuePattern { workspace: Option<WorkspaceSelector<'a>>, pattern: &'a str, action: NetworkCueAction },
    WorkspaceTransport { workspace: Option<WorkspaceSelector<'a>>, action: NetworkWorkspaceAction<'a> },
    /// Use the current native states in the active list under the workspace lock.
    ListPause(NetworkPauseAction),
    /// QLab `/thump` keepalive: acknowledged but performs no action.
    Heartbeat,
    /// Trigger a full cue-list feedback dump.
    CueListRequest,
    /// Trigger a playhead feedback dump.
    PlayheadRequest,
    /// Set or toggle a global engine override (QLab `/overrides/...`).
    SetOverride { key: &'static str, value: Option<bool> },
    /// Address (or its arguments) does not map to any Inkue command.
    Unmatched,
}

fn resolve_action<'a>(addr: &'a str, args: &[rosc::OscType]) -> OscAction<'a> {
    let payload = match addr {
        "/inkue/go"              => serde_json::json!({ "command": "go" }),
        "/inkue/stop"            => serde_json::json!({ "command": "stop_all" }),
        "/inkue/hardstop"        => serde_json::json!({ "command": "hard_stop_all" }),
        "/inkue/pause"           => return OscAction::ListPause(NetworkPauseAction::Pause),
        "/inkue/resume"          => return OscAction::ListPause(NetworkPauseAction::Resume),
        "/inkue/select/next"     => serde_json::json!({ "command": "select_next" }),
        "/inkue/select/previous" => serde_json::json!({ "command": "select_previous" }),
        "/inkue/pause_toggle"    => return OscAction::ListPause(NetworkPauseAction::Toggle),
        "/inkue/cues/request"     => return OscAction::CueListRequest,
        "/inkue/playhead/request" => return OscAction::PlayheadRequest,
        addr if addr.starts_with("/inkue/cue/") => return parse_cue_address(addr, args),
        _ => return parse_qlab_address(addr, args),
    };
    OscAction::Command(payload)
}

fn handle_message(msg: &rosc::OscMessage, explicitly_allowed: bool, app_handle: &tauri::AppHandle) {
    let (matched, activity) = match authorized_action(&msg.addr, &msg.args, explicitly_allowed) {
        OscAction::WorkspaceTransport { workspace, action } => {
            if let Some(state) = app_handle.try_state::<AppState>() {
                if let Err(error) = control_network_workspace(&state, app_handle, action, |show| {
                    workspace.is_none_or(|selector| selector.matches(show))
                }) {
                    log::warn!("Network workspace command {}: {error}", msg.addr);
                }
            }
            (true, true)
        }
        OscAction::AddressedCue { workspace, number, action } => {
            if let Some(state) = app_handle.try_state::<AppState>() {
                if let Err(error) = address_numbered_network_cue(&state, app_handle, action, |show| {
                    resolve_network_target(show, workspace, number)
                }) {
                    log::warn!("Network cue {}: {error}", msg.addr);
                }
            }
            (true, true)
        }
        OscAction::AddressedCuePattern { workspace, pattern, action } => {
            if let Some(state) = app_handle.try_state::<AppState>() {
                if let Err(error) = address_network_cue_pattern(&state, app_handle, action, |show| {
                    resolve_network_pattern(show, workspace, pattern)
                }) {
                    log::warn!("Network cue pattern {}: {error}", msg.addr);
                }
            }
            (true, true)
        }
        OscAction::ListPause(action) => {
            if let Some(state) = app_handle.try_state::<AppState>() {
                if let Err(error) = control_network_list_pause(&state, app_handle, action) {
                    log::warn!("Network list {}: {error}", msg.addr);
                }
            }
            (true, true)
        }
        OscAction::Heartbeat => (true, false),
        OscAction::Command(payload) => {
            let _ = app_handle.emit("osc-command", &payload);
            (true, true)
        }
        OscAction::SetOverride { key, value } => {
            let enabled = value
                .unwrap_or_else(|| !crate::engine::overrides::OVERRIDES.get(key).unwrap_or(true));
            if let Err(error) = crate::engine::overrides::OVERRIDES.set(key, enabled) {
                log::warn!("Override {key}: {error}");
            }
            let _ = app_handle.emit("global-overrides-changed", crate::engine::overrides::OVERRIDES.snapshot());
            (true, true)
        }
        OscAction::CueListRequest => {
            crate::engine::osc_feedback::request_cue_list();
            (true, false)
        }
        OscAction::PlayheadRequest => {
            crate::engine::osc_feedback::request_playhead();
            (true, false)
        }
        OscAction::Unmatched => (false, false),
    };

    // Always emit a debug event regardless of whether the address matches
    // Inkue; `matched` feeds the OSC monitor so it never re-derives the
    // address list on its own.
    let args_display: Vec<String> = msg.args.iter().map(format_osc_arg).collect();
    let _ = app_handle.emit(
        "osc-debug",
        serde_json::json!({
            "addr": msg.addr,
            "args": args_display,
            "matched": matched,
        }),
    );

    if activity {
        let _ = app_handle.emit("osc-activity", serde_json::json!({}));
    }
}

fn authorized_action<'a>(addr: &'a str, args: &[rosc::OscType], explicitly_allowed: bool) -> OscAction<'a> {
    let action = resolve_action(addr, args);
    match action {
        OscAction::AddressedCue { workspace: Some(_), .. }
        | OscAction::AddressedCue { action: NetworkCueAction::Reset, .. }
        | OscAction::AddressedCuePattern { workspace: Some(_), .. }
        | OscAction::AddressedCuePattern { action: NetworkCueAction::Reset, .. }
        | OscAction::WorkspaceTransport { .. }
        | OscAction::SetOverride { .. } if !explicitly_allowed => OscAction::Unmatched,
        // Property writes edit saved show data: control-level only.
        OscAction::AddressedCue { action, .. }
        | OscAction::AddressedCuePattern { action, .. } if !explicitly_allowed && action.is_write() => OscAction::Unmatched,
        _ => action,
    }
}

fn format_osc_arg(arg: &rosc::OscType) -> String {
    match arg {
        rosc::OscType::Int(i)    => format!("i:{i}"),
        rosc::OscType::Float(f)  => format!("f:{f}"),
        rosc::OscType::Double(d) => format!("d:{d}"),
        rosc::OscType::String(s) => format!("s:{s:?}"),
        rosc::OscType::Bool(b)   => format!("b:{b}"),
        rosc::OscType::Long(l)   => format!("l:{l}"),
        rosc::OscType::Blob(b)   => format!("blob({} bytes)", b.len()),
        rosc::OscType::Nil       => "nil".to_string(),
        rosc::OscType::Inf       => "inf".to_string(),
        _                        => "?".to_string(),
    }
}

/// First numeric argument of an OSC message, as f64.
fn numeric_arg(args: &[rosc::OscType]) -> Option<f64> {
    args.iter().find_map(|a| match a {
        rosc::OscType::Float(f)  => Some(*f as f64),
        rosc::OscType::Double(d) => Some(*d),
        rosc::OscType::Int(i)    => Some(*i as f64),
        rosc::OscType::Long(l)   => Some(*l as f64),
        _ => None,
    })
}

/// Parse `/inkue/cue/{number}/<action>`.
///
/// Actions:
/// - `go` / `start` / `select` / `stop` — no argument.
/// - `seek <seconds>` — absolute position within the clip.
/// - `seek/relative <±seconds>` — jump from the current position.
/// - `seek/percent <0..1>` — fraction of the clip (fader-friendly).
fn parse_cue_address<'a>(addr: &'a str, args: &[rosc::OscType]) -> OscAction<'a> {
    let parts: Vec<&str> = addr.splitn(7, '/').collect();
    // parts: ["", "inkue", "cue", "{number}", "action", ("seek mode")]
    if parts.len() < 5 {
        return OscAction::Unmatched;
    }
    let number = parts[3];
    let action = parts[4];

    if action == "seek" {
        let mode = match parts.get(5) {
            None => "absolute",
            Some(&"relative") => "relative",
            Some(&"percent") => "percent",
            Some(_) => return OscAction::Unmatched,
        };
        let Some(value) = numeric_arg(args) else {
            return OscAction::Unmatched;
        };
        return OscAction::Command(serde_json::json!({
            "command": "cue_seek",
            "cue_number": number,
            "seek_mode": mode,
            "value": value,
        }));
    }

    if parts.len() != 5 {
        return OscAction::Unmatched;
    }
    if !literal_cue_number(number) {
        return OscAction::Unmatched;
    }
    let action = match action {
        "go" => NetworkCueAction::Go,
        "start" if args.is_empty() => NetworkCueAction::Start,
        "select" if args.is_empty() => NetworkCueAction::Select,
        "stop" if args.is_empty() => NetworkCueAction::Stop,
        "pause" if args.is_empty() => NetworkCueAction::SetPause(NetworkPauseAction::Pause),
        "resume" if args.is_empty() => NetworkCueAction::SetPause(NetworkPauseAction::Resume),
        _ => return OscAction::Unmatched,
    };
    OscAction::AddressedCue { workspace: None, number, action }
}

fn literal_cue_number(number: &str) -> bool {
    !number.is_empty() && !number.chars().any(|c| {
        c.is_whitespace() || c.is_control() || matches!(c, '/' | '*' | '?' | '[' | ']' | '{' | '}')
    })
}

fn qlab_literal_cue_number(number: &str) -> bool {
    literal_cue_number(number)
        && !matches!(number, "selected" | "active" | "playhead" | "all" | "none" | "next" | "previous" | "nextSequence" | "previousSequence")
}

/// QLab cue verbs that write a saved cue field and therefore take exactly one
/// argument: `/cue/{n}/preWait 5`, `/cue/{n}/armed 0`, ...
fn cue_write_action(verb: &str, args: &[rosc::OscType]) -> Option<NetworkCueAction> {
    if args.len() != 1 {
        return None;
    }
    match verb {
        "preWait" | "postWait" => {
            let value = numeric_arg(args)?;
            (value.is_finite() && value >= 0.0).then(|| {
                if verb == "preWait" { NetworkCueAction::SetPreWait(value) } else { NetworkCueAction::SetPostWait(value) }
            })
        }
        "armed" => qlab_bool(&args[0]).map(NetworkCueAction::SetArmed),
        "qName" => match &args[0] {
            rosc::OscType::String(name)
                if !name.is_empty() && name.len() <= MAX_CUE_ADDRESS_NUMBER_BYTES =>
            {
                Some(NetworkCueAction::SetQName(name.clone()))
            }
            _ => None,
        },
        "continueMode" => match &args[0] {
            rosc::OscType::String(mode) => serde_json::from_str::<crate::cue::types::ContinueMode>(
                &serde_json::to_string(mode).ok()?,
            ).ok().map(NetworkCueAction::SetContinueMode),
            _ => None,
        },
        _ => None,
    }
}

fn parse_qlab_address<'a>(addr: &'a str, args: &[rosc::OscType]) -> OscAction<'a> {
    if let Some(rest) = addr.strip_prefix("/overrides/") {
        return parse_override_address(rest, args);
    }
    // Property writes are the only cue commands that carry an argument.
    let cue_write = addr
        .strip_prefix("/cue/")
        .or_else(|| addr.strip_prefix("/cue_id/"))
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(_, verb)| matches!(verb, "preWait" | "postWait" | "armed" | "qName" | "continueMode"));
    if !args.is_empty() && !cue_write {
        return OscAction::Unmatched;
    }
    let Some((workspace, addr)) = WorkspaceSelector::split_address(addr) else {
        return OscAction::Unmatched;
    };
    use NetworkWorkspaceAction as Transport;
    if addr == "/thump" {
        return OscAction::Heartbeat;
    }
    // QLab `/go/{cue_number}`: park the playhead on the numbered cue, then GO.
    if let Some(number) = addr.strip_prefix("/go/") {
        return if literal_cue_number(number) {
            OscAction::WorkspaceTransport { workspace, action: Transport::GoToCue(number) }
        } else {
            OscAction::Unmatched
        };
    }
    let transport = match addr {
        "/go" => Some(Transport::Go),
        "/stop" | "/panic" => Some(Transport::Stop),
        "/hardStop" => Some(Transport::HardStop),
        "/pause" => Some(Transport::SetPaused(true)),
        "/resume" => Some(Transport::SetPaused(false)),
        "/reset" => Some(Transport::Reset),
        _ => None,
    };
    if let Some(action) = transport {
        return OscAction::WorkspaceTransport { workspace, action };
    }
    let playhead = addr.strip_prefix("/playhead/").or_else(|| addr.strip_prefix("/playbackPosition/"));
    let (number, action) = if let Some(number) = playhead {
        let action = match number {
            "next" => Some(Transport::Step { forward: true, sequence: false }),
            "previous" => Some(Transport::Step { forward: false, sequence: false }),
            "nextSequence" => Some(Transport::Step { forward: true, sequence: true }),
            "previousSequence" => Some(Transport::Step { forward: false, sequence: true }),
            "none" => Some(Transport::ClearStandby),
            _ => None,
        };
        if let Some(action) = action {
            return OscAction::WorkspaceTransport { workspace, action };
        }
        if workspace.is_some() {
            return OscAction::Unmatched;
        }
        (number, NetworkCueAction::Select)
    } else if let Some((id, action)) = addr.strip_prefix("/cue_id/").and_then(|rest| rest.split_once('/')) {
        // QLab `/cue_id/{uniqueID}/...`: the address segment is a cue UUID.
        if hyphenated_query_id(id).is_none() {
            return OscAction::Unmatched;
        }
        match action {
            "start" if args.is_empty() => (id, NetworkCueAction::Start),
            "stop" if args.is_empty() => (id, NetworkCueAction::Stop),
            "reset" if args.is_empty() => (id, NetworkCueAction::Reset),
            "pause" if args.is_empty() => (id, NetworkCueAction::SetPause(NetworkPauseAction::Pause)),
            "resume" if args.is_empty() => (id, NetworkCueAction::SetPause(NetworkPauseAction::Resume)),
            verb => match cue_write_action(verb, args) {
                Some(write) => (id, write),
                None => return OscAction::Unmatched,
            },
        }
    } else if let Some((number, action)) = addr.strip_prefix("/cue/").and_then(|cue| cue.split_once('/')) {
        if number.contains('*') {
            // Wildcard target: only stop/pause/resume/reset and property writes;
            // never start or select every cue at once.
            let action = match action {
                "stop" if args.is_empty() => NetworkCueAction::Stop,
                "reset" if args.is_empty() => NetworkCueAction::Reset,
                "pause" if args.is_empty() => NetworkCueAction::SetPause(NetworkPauseAction::Pause),
                "resume" if args.is_empty() => NetworkCueAction::SetPause(NetworkPauseAction::Resume),
                verb => match cue_write_action(verb, args) {
                    Some(write) if write.is_write() => write,
                    _ => return OscAction::Unmatched,
                },
            };
            if workspace.is_some() && number.len() > MAX_CUE_ADDRESS_NUMBER_BYTES {
                return OscAction::Unmatched;
            }
            return OscAction::AddressedCuePattern { workspace, pattern: number, action };
        }
        match action {
            "start" if args.is_empty() => (number, NetworkCueAction::Start),
            "stop" if args.is_empty() => (number, NetworkCueAction::Stop),
            "reset" if args.is_empty() => (number, NetworkCueAction::Reset),
            "pause" if args.is_empty() => (number, NetworkCueAction::SetPause(NetworkPauseAction::Pause)),
            "resume" if args.is_empty() => (number, NetworkCueAction::SetPause(NetworkPauseAction::Resume)),
            verb => match cue_write_action(verb, args) {
                Some(write) => (number, write),
                None => return OscAction::Unmatched,
            },
        }
    } else {
        return OscAction::Unmatched;
    };
    // `playhead` and `selected` are reserved QLab identifiers (Inkue's single
    // selection is the playhead); a UUID-shaped segment addresses a cue by ID.
    let reserved = matches!(number, "playhead" | "selected");
    if (!reserved && !qlab_literal_cue_number(number))
        || (workspace.is_some() && number.len() > MAX_CUE_ADDRESS_NUMBER_BYTES)
    {
        return OscAction::Unmatched;
    }
    OscAction::AddressedCue { workspace, number, action }
}

/// QLab application-level `/overrides/{toggleX|xxEnabled}[/toggle] [bool]`.
/// With no argument (or `toggle`) the current value flips; otherwise the
/// argument follows QLab's boolean rules (number != 0, or a string starting
/// with Y/y/T/t/1-9 true, N/n/F/f/0 false).
fn parse_override_address(rest: &str, args: &[rosc::OscType]) -> OscAction<'static> {
    let (name, suffix) = match rest.split_once('/') {
        Some((name, suffix)) => (name, Some(suffix)),
        None => (rest, None),
    };
    if suffix.is_some() && suffix != Some("toggle") {
        return OscAction::Unmatched;
    }
    let (core, toggle_name) = if let Some(core) = name.strip_prefix("toggle") {
        (core, true)
    } else if let Some(core) = name.strip_suffix("Enabled") {
        (core, false)
    } else {
        return OscAction::Unmatched;
    };
    // The `toggle` prefix leaves the core capitalized ("DmxOutput"); the
    // `Enabled` suffix form keeps QLab's camelCase ("dmxOutput").
    let key = match core {
        "dmxOutput" | "DmxOutput" => "dmx_output",
        "midiInput" | "MidiInput" => "midi_input",
        "midiOutput" | "MidiOutput" => "midi_output",
        "mscInput" | "MscInput" => "msc_input",
        "mscOutput" | "MscOutput" => "msc_output",
        "networkExternalInput" | "NetworkExternalInput" => "network_external_input",
        "timecodeInput" | "TimecodeInput" => "timecode_input",
        _ => return OscAction::Unmatched,
    };
    let value = match args {
        [] => None,
        _ if toggle_name => return OscAction::Unmatched,
        [one] => match one {
            rosc::OscType::String(s) if s.eq_ignore_ascii_case("toggle") => None,
            other => match qlab_bool(other) {
                Some(value) => Some(value),
                None => return OscAction::Unmatched,
            },
        },
        _ => return OscAction::Unmatched,
    };
    OscAction::SetOverride { key, value }
}

/// QLab boolean coercion: any nonzero number is true; strings follow the
/// yes/no/true/false prefix rules.
fn qlab_bool(arg: &rosc::OscType) -> Option<bool> {
    if let Some(number) = numeric_arg(std::slice::from_ref(arg)) {
        return Some(number != 0.0);
    }
    if let rosc::OscType::String(text) = arg {
        let first = text.chars().next()?;
        return Some(match first {
            'N' | 'n' | 'F' | 'f' | '0' => false,
            'Y' | 'y' | 'T' | 't' | '1'..='9' => true,
            _ => return None,
        });
    }
    if let rosc::OscType::Bool(b) = arg {
        return Some(*b);
    }
    None
}

const MAX_CUE_ADDRESS_NUMBER_BYTES: usize = 96;
const MAX_WORKSPACE_SELECTOR_BYTES: usize = 255;
const MAX_SCOPED_ROWS: usize = 4_096;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CueQueryProperty {
    Number,
    UniqueId,
    Running,
    Paused,
    QName,
    Armed,
    ContinueMode,
    PreWait,
    Duration,
    CueType,
}

#[derive(Clone, Copy)]
enum CueQueryTarget<'a> {
    Number(&'a str),
    Id(Uuid),
    Playhead,
}

#[derive(Clone, Copy)]
enum WorkspaceSelector<'a> {
    Instance(Uuid),
    Name(&'a str),
}

impl<'a> WorkspaceSelector<'a> {
    fn split_address(address: &'a str) -> Option<(Option<Self>, &'a str)> {
        let Some(scoped) = address.strip_prefix("/workspace/") else {
            return Some((None, address));
        };
        let separator = scoped.find('/')?;
        let selector = Self::parse(&scoped[..separator])?;
        Some((Some(selector), &scoped[separator..]))
    }

    fn parse(segment: &'a str) -> Option<Self> {
        if let Some(id) = hyphenated_query_id(segment) {
            return Some(Self::Instance(id));
        }
        if !segment.is_empty() && segment.len() <= MAX_WORKSPACE_SELECTOR_BYTES
            && segment.bytes().all(|byte| byte.is_ascii_graphic() && !b"#*,/?[]{}".contains(&byte))
        {
            return Some(Self::Name(segment));
        }
        None
    }

    fn matches(self, workspace: &Workspace) -> bool {
        match self {
            Self::Instance(id) => workspace.instance_id() == id,
            Self::Name(name) => workspace.metadata.name == name
                || workspace.file_path.as_deref().and_then(std::path::Path::file_name)
                    .and_then(std::ffi::OsStr::to_str) == Some(name),
        }
    }
}

/// QLab wildcard match: `*` matches any (possibly empty) run of characters;
/// every other character is literal. No `*` means an exact match.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Resolve a wildcard cue address to every matching top-level cue. Unscoped
/// patterns search the active list; workspace-scoped patterns search every
/// list's top level (never nested children, so a remote can't bypass a
/// parent group's transport).
fn resolve_network_pattern(
    show: &Workspace,
    workspace: Option<WorkspaceSelector<'_>>,
    pattern: &str,
) -> Vec<(usize, CueId)> {
    let mut out = Vec::new();
    let mut push_matching = |index: usize, cues: &[Box<dyn Cue>]| {
        if out.len() + cues.len() > MAX_SCOPED_ROWS {
            return;
        }
        for cue in cues {
            if cue.number().is_some_and(|number| glob_match(pattern, number)) {
                out.push((index, cue.id()));
            }
        }
    };
    if let Some(selector) = workspace {
        if !selector.matches(show) {
            return out;
        }
        for (index, list) in show.cue_lists.iter().enumerate() {
            push_matching(index, &list.cues);
        }
    } else {
        let Some(index) = show.cue_lists.iter().position(|list| list.id == show.active_cue_list_id) else {
            return out;
        };
        push_matching(index, &show.cue_lists[index].cues);
    }
    out
}

fn resolve_network_target(
    show: &Workspace,
    workspace: Option<WorkspaceSelector<'_>>,
    number: &str,
) -> Option<(usize, CueId)> {
    // QLab reserved identifiers: `playhead` and `selected` both mean the standby
    // cue of the active list (Inkue keeps a single selection, the playhead).
    if matches!(number, "playhead" | "selected") {
        if workspace.is_some_and(|selector| !selector.matches(show)) {
            return None;
        }
        let index = show.cue_lists.iter().position(|list| list.id == show.active_cue_list_id)?;
        let cue = show.cue_lists[index].playhead_cue()?;
        return Some((index, cue.id()));
    }
    // A UUID-shaped segment (from `/cue_id/...`) addresses a cue by unique ID.
    // IDs are workspace-wide, but only top-level cues can be controlled directly.
    if let Some(id) = hyphenated_query_id(number) {
        if workspace.is_some_and(|selector| !selector.matches(show)) {
            return None;
        }
        let (index, cue) = unique_workspace_cue(show, |cue| cue.id() == id)?;
        return show.cue_lists[index].cues.iter().any(|row| std::ptr::eq(row.as_ref(), cue))
            .then(|| (index, cue.id()));
    }
    if let Some(selector) = workspace {
        if !selector.matches(show) {
            return None;
        }
        let (index, cue) = unique_workspace_cue(show, |cue| cue.number() == Some(number))?;
        // Count group children when checking uniqueness, but only control
        // top-level cues so a remote can't bypass a parent group's transport.
        show.cue_lists[index].cues.iter().any(|row| std::ptr::eq(row.as_ref(), cue))
            .then(|| (index, cue.id()))
    } else {
        let index = show.cue_lists.iter().position(|list| list.id == show.active_cue_list_id)?;
        let cue = show.cue_lists[index].unique_numbered_cue(number)?;
        Some((index, cue.id()))
    }
}

enum CueQueryKind<'a> {
    Cue { target: CueQueryTarget<'a>, property: CueQueryProperty },
    /// QLab `/cueLists` (optionally workspace-scoped): every cue list in the workspace.
    CueLists,
    /// QLab `/cueList/{list_id}/playbackPosition`: the standby cue ID, or null.
    CueListPlaybackPosition(Uuid),
}

struct CueQuery<'a> {
    address: &'a str,
    workspace: Option<WorkspaceSelector<'a>>,
    kind: CueQueryKind<'a>,
}

impl<'a> CueQuery<'a> {
    fn parse(message: &'a rosc::OscMessage) -> Option<Self> {
        if !message.args.is_empty() {
            return None;
        }
        let (workspace, path) = WorkspaceSelector::split_address(&message.addr)?;
        let path = path.strip_prefix('/')?;
        // Workspace-level queries first: `/cueLists`, `/cueList/{id}/playbackPosition`.
        if path == "cueLists" {
            return Some(Self { address: &message.addr, workspace, kind: CueQueryKind::CueLists });
        }
        if let Some(rest) = path.strip_prefix("cueList/") {
            let (id, property) = rest.split_once('/')?;
            if property != "playbackPosition" {
                return None;
            }
            return Some(Self {
                address: &message.addr,
                workspace,
                kind: CueQueryKind::CueListPlaybackPosition(hyphenated_query_id(id)?),
            });
        }
        let (kind, address) = path.split_once('/')?;
        let (key, property) = address.split_once('/')?;
        let property = match property {
            "number" => CueQueryProperty::Number,
            "uniqueID" => CueQueryProperty::UniqueId,
            "isRunning" => CueQueryProperty::Running,
            "isPaused" => CueQueryProperty::Paused,
            "qName" => CueQueryProperty::QName,
            "armed" => CueQueryProperty::Armed,
            "continueMode" => CueQueryProperty::ContinueMode,
            "preWait" => CueQueryProperty::PreWait,
            "duration" => CueQueryProperty::Duration,
            "qType" => CueQueryProperty::CueType,
            _ => return None,
        };
        let target = match kind {
            "cue" if key == "playhead"
                && matches!(property, CueQueryProperty::Number | CueQueryProperty::UniqueId) =>
                CueQueryTarget::Playhead,
            "cue" if key.len() <= MAX_CUE_ADDRESS_NUMBER_BYTES && qlab_literal_cue_number(key) =>
                CueQueryTarget::Number(key),
            "cue_id" => CueQueryTarget::Id(hyphenated_query_id(key)?),
            _ => return None,
        };
        Some(Self {
            address: &message.addr,
            workspace,
            kind: CueQueryKind::Cue { target, property },
        })
    }

    fn is_single_datagram(&self, data: &[u8]) -> bool {
        // The decoder accepts some malformed empty type tags. Require the exact
        // address padding and zero-argument OSC type tag before returning data.
        let end = (self.address.len() + 1).next_multiple_of(4);
        data.get(self.address.len()..end)
            .is_some_and(|padding| padding.iter().all(|byte| *byte == 0))
            && data.get(end..) == Some(b",\0\0\0")
    }

    fn read(&self, workspace: &Workspace) -> Option<serde_json::Value> {
        if workspace.cue_lists.len() > MAX_SCOPED_ROWS {
            return None;
        }
        match &self.kind {
            CueQueryKind::CueLists => Some(serde_json::Value::Array(
                workspace.cue_lists.iter().map(|list| serde_json::json!({
                    "id": list.id.to_string(),
                    "name": list.name,
                })).collect(),
            )),
            CueQueryKind::CueListPlaybackPosition(list_id) => {
                let list = workspace.cue_lists.iter().find(|list| list.id == *list_id)?;
                Some(match list.playhead_cue_id {
                    Some(id) => serde_json::Value::String(id.to_string()),
                    None => serde_json::Value::Null,
                })
            }
            CueQueryKind::Cue { target, property } => self.read_cue(workspace, *target, *property),
        }
    }

    fn read_cue(
        &self,
        workspace: &Workspace,
        target: CueQueryTarget<'_>,
        property: CueQueryProperty,
    ) -> Option<serde_json::Value> {
        let cue = match target {
            CueQueryTarget::Playhead => {
                let list = workspace.active_cue_list().filter(|list| list.cues.len() <= MAX_SCOPED_ROWS)?;
                let playhead = list.playhead_cue()?;
                if playhead.group_mode() == Some(crate::cue::types::GroupMode::StartFirstAndEnter) && !playhead.is_disabled() {
                    let children = playhead.child_cues().filter(|children| children.len() <= MAX_SCOPED_ROWS)?;
                    let next = playhead.active_child_id();
                    children.iter().map(|child| child.as_ref()).find(|child| Some(child.id()) == next)
                        .or(Some(playhead))
                } else {
                    Some(playhead)
                }
            }
            CueQueryTarget::Number(number) if self.workspace.is_none() => {
                // Keep the existing short address scoped to the active list.
                let list = workspace.active_cue_list().filter(|list| list.cues.len() <= MAX_SCOPED_ROWS)?;
                list.unique_numbered_cue(number)
            }
            CueQueryTarget::Number(number) => unique_workspace_cue(workspace, |cue| cue.number() == Some(number)).map(|(_, cue)| cue),
            CueQueryTarget::Id(id) => unique_workspace_cue(workspace, |cue| cue.id() == id).map(|(_, cue)| cue),
        }?;
        match property {
            CueQueryProperty::Number => cue.number()
                .filter(|number| number.len() <= MAX_CUE_ADDRESS_NUMBER_BYTES)
                .map(|number| serde_json::Value::String(number.to_owned())),
            CueQueryProperty::UniqueId => Some(serde_json::Value::String(cue.id().to_string())),
            CueQueryProperty::Running => Some(serde_json::Value::Bool(cue.is_running())),
            CueQueryProperty::Paused => Some(serde_json::Value::Bool(cue.is_paused())),
            CueQueryProperty::QName => (cue.name().len() <= MAX_CUE_ADDRESS_NUMBER_BYTES)
                .then(|| serde_json::Value::String(cue.name().to_owned())),
            CueQueryProperty::Armed => Some(serde_json::Value::Bool(!cue.is_disabled())),
            CueQueryProperty::ContinueMode => serde_json::to_value(cue.continue_mode()).ok(),
            CueQueryProperty::PreWait => Some(serde_json::Value::from(cue.pre_wait().as_secs_f64())),
            CueQueryProperty::Duration => Some(match cue.duration() {
                Some(duration) => serde_json::Value::from(duration.as_secs_f64()),
                None => serde_json::Value::Null,
            }),
            CueQueryProperty::CueType => serde_json::to_value(cue.cue_type()).ok(),
        }
    }

    fn reply_data(&self, workspace: &Workspace) -> Option<(Uuid, Option<serde_json::Value>)> {
        let instance = workspace.instance_id();
        if self.workspace.is_some_and(|requested| !requested.matches(workspace)) {
            return None;
        }
        Some((instance, self.read(workspace)))
    }

    fn encode_reply(&self, workspace_id: Uuid, data: Option<serde_json::Value>) -> Option<Vec<u8>> {
        let status = if data.is_some() { "ok" } else { "error" };
        let json = serde_json::json!({ "workspace_id": workspace_id, "address": self.address, "status": status, "data": data });
        rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: format!("/reply{}", self.address),
            args: vec![rosc::OscType::String(json.to_string())],
        })).ok()
    }
}

fn hyphenated_query_id(id: &str) -> Option<Uuid> {
    if id.len() != 36 {
        return None;
    }
    id.parse().ok()
}

/// Bound the full search before a result can be trusted as unique. Iterate
/// groups without adding call-stack depth while the workspace is locked.
fn unique_workspace_cue(
    workspace: &Workspace,
    matches: impl Fn(&dyn Cue) -> bool,
) -> Option<(usize, &dyn Cue)> {
    if workspace.cue_lists.len() > MAX_SCOPED_ROWS {
        return None;
    }
    let mut remaining = MAX_SCOPED_ROWS;
    let mut pending = Vec::new();
    let mut found = None;
    for (index, list) in workspace.cue_lists.iter().enumerate() {
        remaining = remaining.checked_sub(list.cues.len())?;
        pending.extend(list.cues.iter().map(Box::as_ref));
        while let Some(cue) = pending.pop() {
            if matches(cue) && found.replace((index, cue)).is_some() {
                return None;
            }
            if let Some(children) = cue.child_cues() {
                remaining = remaining.checked_sub(children.len())?;
                pending.extend(children.iter().map(Box::as_ref));
            }
        }
    }
    found
}

fn handle_query(query: CueQuery<'_>, handle: &tauri::AppHandle) -> Option<Vec<u8>> {
    let state = handle.try_state::<AppState>()?;
    let (workspace_id, data) = {
        // A read never blocks show work. A busy desk sends no reply; the peer can retry.
        let workspace = state.workspace.try_lock().ok()?;
        query.reply_data(&workspace)?
    };
    query.encode_reply(workspace_id, data)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn query_message(address: &str) -> rosc::OscMessage {
        rosc::OscMessage { addr: address.into(), args: vec![] }
    }

    fn query_packet(address: &str) -> Vec<u8> {
        rosc::encoder::encode(&rosc::OscPacket::Message(query_message(address))).unwrap()
    }

    #[test]
    fn query_scrutinizes_addresses_without_enabling_qlab_writes_or_text_queries() {
        for address in ["/cue/101/number", "/cue/101/isRunning", "/cue/101/isPaused", "/cue/playhead/number"] {
            let mut message = query_message(address);
            assert!(CueQuery::parse(&message).is_some());
            message.args.push(rosc::OscType::String("102".into()));
            assert!(CueQuery::parse(&message).is_none());
            assert!(parse_text_message(address.as_bytes()).is_none());
        }
        for address in ["/cue/*/isRunning", "/cue/playhead/isRunning", "/cue/selected/number",
            "/cue/1/isRunning/extra", "/workspace/show name/cue/1/number",
            "/workspace/show*/cue/1/number", "/workspace//cue/1/number", "/cue//number"]
        {
            assert!(CueQuery::parse(&query_message(address)).is_none(), "{address}");
        }
        let oversized = format!("/cue/{}/number", "1".repeat(MAX_CUE_ADDRESS_NUMBER_BYTES + 1));
        assert!(CueQuery::parse(&query_message(&oversized)).is_none());
        let oversized = format!("/workspace/{}/cue/101/number", "a".repeat(MAX_WORKSPACE_SELECTOR_BYTES + 1));
        assert!(CueQuery::parse(&query_message(&oversized)).is_none());
    }

    #[test]
    fn queries_read_cue_lists_and_playback_position_per_list() {
        use crate::cue::{memo_cue::MemoCue, traits::Cue, wait_cue::WaitCue};

        let mut workspace = Workspace::new("Lists");
        let active_id = workspace.active_cue_list_id;
        let mut standby = WaitCue::new();
        standby.set_number(Some("5".into()));
        let standby_id = standby.id();
        workspace.active_cue_list_mut().unwrap().push(Box::new(standby));
        workspace.active_cue_list_mut().unwrap().set_playhead(Some(standby_id)).unwrap();
        let mut other = crate::show::cue_list::CueList::new("Cart");
        other.push(Box::new(MemoCue::new()));
        let other_id = other.id;
        workspace.cue_lists.push(other);
        workspace.cue_lists.last_mut().unwrap().set_playhead(None).unwrap();

        let read = |address: &str| CueQuery::parse(&query_message(address)).unwrap().read(&workspace);
        let lists = read("/cueLists").unwrap();
        assert_eq!(lists.as_array().unwrap().len(), 2);
        assert_eq!(lists[0]["id"], active_id.to_string());
        assert_eq!(lists[0]["name"], serde_json::Value::String("Cue List 1".into()));
        assert_eq!(lists[1]["name"], serde_json::Value::String("Cart".into()));

        let position = read(&format!("/cueList/{active_id}/playbackPosition")).unwrap();
        assert_eq!(position, serde_json::Value::String(standby_id.to_string()));
        let empty = read(&format!("/cueList/{other_id}/playbackPosition")).unwrap();
        assert_eq!(empty, serde_json::Value::Null);

        // Workspace-scoped forms work; wrong workspace selector reads nothing.
        let scoped = format!("/workspace/Lists/cueLists");
        assert_eq!(CueQuery::parse(&query_message(&scoped)).unwrap().read(&workspace).unwrap().as_array().unwrap().len(), 2);
        let wrong = format!("/workspace/Other/cueLists");
        assert!(CueQuery::parse(&query_message(&wrong)).unwrap().reply_data(&workspace).is_none());

        // Unknown shapes stay unmatched.
        for address in ["/cueList/abc/playbackPosition", "/cueLists/extra", "/cueList/", "/cueLists 1"] {
            let args: &[rosc::OscType] = if address.ends_with('1') { &[rosc::OscType::Int(1)] } else { &[] };
            assert!(CueQuery::parse(&rosc::OscMessage { addr: address.trim_end_matches('1').trim_end().into(), args: args.to_vec() }).is_none(), "{address}");
        }
    }

    #[test]
    fn queries_read_qlab_cue_properties_from_the_active_list() {
        use crate::cue::{memo_cue::MemoCue, traits::Cue, types::ContinueMode};

        let mut workspace = Workspace::new("QueryProps");
        let mut cue = MemoCue::new();
        cue.set_number(Some("42".into()));
        cue.set_name("LX warmup".into());
        cue.set_disabled(true);
        cue.set_pre_wait(std::time::Duration::from_secs(3));
        cue.set_continue_mode(ContinueMode::AutoFollow);
        let id = cue.id();
        workspace.active_cue_list_mut().unwrap().push(Box::new(cue));

        let read = |address: &str| {
            CueQuery::parse(&query_message(address)).unwrap().read(&workspace)
        };
        assert_eq!(read("/cue/42/qName"), Some("LX warmup".into()));
        assert_eq!(read("/cue/42/armed"), Some(false.into()));
        assert_eq!(read("/cue/42/continueMode"), Some("auto_follow".into()));
        assert_eq!(read("/cue/42/preWait"), Some(3.0.into()));
        assert_eq!(read("/cue/42/qType"), Some("memo".into()));
        assert_eq!(read("/cue/42/duration"), Some(serde_json::Value::Null));
        assert_eq!(read(&format!("/cue_id/{id}/isRunning")), Some(false.into()));
        assert!(CueQuery::parse(&query_message("/cue/42/level")).is_none(), "write-only properties stay unmatched");
        assert!(CueQuery::parse(&query_message("/cue/42/qName/extra")).is_none());
    }

    #[test]
    fn query_reads_the_active_list_without_editing_it_and_refuses_ambiguous_numbers() {
        use crate::cue::{memo_cue::MemoCue, traits::Cue};

        let mut workspace = Workspace::new("QueryPractice");
        let revision = workspace.revision;
        let list = workspace.active_cue_list_mut().unwrap();
        list.auto_renumber = false;
        let mut first = MemoCue::new();
        first.set_number(Some("101".into()));
        list.push(Box::new(first));

        let read = |address: &str, workspace: &Workspace| {
            CueQuery::parse(&query_message(address)).unwrap().read(workspace)
        };
        assert_eq!(read("/cue/playhead/number", &workspace), Some("101".into()));
        assert_eq!(read("/cue/101/isRunning", &workspace), Some(false.into()));
        assert_eq!(read("/cue/101/isPaused", &workspace), Some(false.into()));
        assert_eq!(read("/cue/102/number", &workspace), None);
        assert_eq!(workspace.revision, revision);

        let mut duplicate = MemoCue::new();
        duplicate.set_number(Some("101".into()));
        workspace.active_cue_list_mut().unwrap().push(Box::new(duplicate));
        assert_eq!(read("/cue/101/number", &workspace), None);
        assert_eq!(read("/cue/playhead/number", &workspace), Some("101".into()));
    }

    #[test]
    fn playhead_query_reports_the_exact_enter_child() {
        use crate::cue::{group_cue::GroupCue, memo_cue::MemoCue, types::GroupMode};

        let mut workspace = Workspace::new("Deck");
        let list = workspace.active_cue_list_mut().unwrap();
        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirstAndEnter;
        group.set_number(Some("3".into()));
        let mut first = MemoCue::new();
        first.set_number(Some("3.1".into()));
        let mut last = MemoCue::new();
        last.set_number(Some("3.2".into()));
        let last_id = last.id();
        group.children = vec![Box::new(first), Box::new(last)];
        list.push(Box::new(group));
        list.set_playhead(Some(last_id)).unwrap();

        let read = |address: &str| CueQuery::parse(&query_message(address)).unwrap().read(&workspace);
        assert_eq!(read("/cue/playhead/number"), Some("3.2".into()));
        assert_eq!(read("/workspace/Deck/cue/playhead/uniqueID"), Some(last_id.to_string().into()));
        assert_eq!(read("/cue/3.2/isRunning"), None, "short numbered queries keep their scope");
    }

    #[test]
    fn scoped_queries_resolve_nested_cues_and_reject_stale_workspace_ids() {
        use crate::cue::{group_cue::GroupCue, memo_cue::MemoCue};
        use crate::show::cue_list::CueList;

        let mut workspace = Workspace::new("QueryPractice");
        let instance = workspace.instance_id();
        let mut active = MemoCue::new();
        active.set_number(Some("101".into()));
        let top_level = active.id();
        workspace.active_cue_list_mut().unwrap().push(Box::new(active));

        let mut child = MemoCue::new();
        child.set_number(Some("102.1".into()));
        let nested = child.id();
        let unnumbered = MemoCue::new();
        let unnumbered_id = unnumbered.id();
        let mut group = GroupCue::new();
        group.children.extend([Box::new(child) as Box<dyn Cue>, Box::new(unnumbered)]);
        let mut other_list = CueList::new("Other list");
        other_list.push(Box::new(group));
        let revision = workspace.revision;
        workspace.cue_lists.push(other_list);

        let query = |path: &str, workspace: &Workspace| {
            CueQuery::parse(&query_message(path)).unwrap().reply_data(workspace)
        };
        let scoped = |path| format!("/workspace/{}/cue/{path}", instance.to_string().to_uppercase());
        assert_eq!(query("/cue/102.1/uniqueID", &workspace), Some((instance, None)));
        assert_eq!(query(&scoped("102.1/uniqueID"), &workspace), Some((instance, Some(nested.to_string().into()))));
        assert_eq!(query("/workspace/QueryPractice/cue/102.1/uniqueID", &workspace), Some((instance, Some(nested.to_string().into()))));
        assert_eq!(query("/workspace/QueryPractice.inkue/cue/102.1/uniqueID", &workspace), None);
        workspace.file_path = Some("/show/QueryPractice.inkue".into());
        assert_eq!(query("/workspace/QueryPractice.inkue/cue/102.1/uniqueID", &workspace), Some((instance, Some(nested.to_string().into()))));
        assert_eq!(query("/workspace/QueryPractice.qlab5/cue/102.1/uniqueID", &workspace), None);
        assert_eq!(query("/workspace/QueryPractice.INKUE/cue/102.1/uniqueID", &workspace), None);
        assert_eq!(query("/workspace/querypractice/cue/102.1/uniqueID", &workspace), None);
        assert_eq!(query(&format!("/cue_id/{nested}/number"), &workspace), Some((instance, Some("102.1".into()))));
        assert_eq!(query(&format!("/workspace/{instance}/cue_id/{unnumbered_id}/isPaused"), &workspace), Some((instance, Some(false.into()))));
        assert_eq!(query(&format!("/cue_id/{unnumbered_id}/number"), &workspace), Some((instance, None)));
        assert_eq!(query(&scoped("playhead/uniqueID"), &workspace), Some((instance, Some(top_level.to_string().into()))));
        assert_eq!(query(&format!("/workspace/{}/cue/101/number", Uuid::new_v4()), &workspace), None);
        assert_eq!(workspace.revision, revision);

        let mut duplicate = MemoCue::new();
        duplicate.set_number(Some("101".into()));
        workspace.cue_lists[1].push(Box::new(duplicate));
        assert_eq!(query("/cue/101/number", &workspace), Some((instance, Some("101".into()))));
        assert_eq!(query(&scoped("101/number"), &workspace), Some((instance, None)));

        let write = format!("/workspace/{instance}/cue_id/{nested}/start");
        assert!(CueQuery::parse(&query_message(&write)).is_none());
        let OscAction::AddressedCue { workspace: scope, number, action } = resolve_action(&write, &[]) else {
            panic!("cue_id writes resolve as addressed cue actions");
        };
        assert_eq!(action, NetworkCueAction::Start);
        assert_eq!(resolve_network_target(&workspace, scope, number), None, "nested cue IDs stay uncontrollable");
        assert!(CueQuery::parse(&query_message("/workspace/QueryPractice/cue/101/start")).is_none());

        workspace.metadata.name = "VenueB".into();
        workspace.file_path = Some("/show/VenueB.inkue".into());
        assert_eq!(query("/workspace/QueryPractice/cue/102.1/number", &workspace), None);
        assert_eq!(query("/workspace/QueryPractice.inkue/cue/102.1/number", &workspace), None);
        assert_eq!(query("/workspace/VenueB/cue/102.1/number", &workspace), Some((instance, Some("102.1".into()))));
        let uuid_shaped_name = Uuid::new_v4();
        workspace.metadata.name = uuid_shaped_name.to_string();
        assert_eq!(query(&format!("/workspace/{uuid_shaped_name}/cue/102.1/number"), &workspace), None);
    }

    #[test]
    fn workspace_scoped_commands_resolve_only_unique_top_level_cues_in_the_requested_show() {
        use crate::cue::{group_cue::GroupCue, types::ContinueMode, wait_cue::WaitCue};
        use crate::show::{cue_list::CueList, transport::Transport};

        let mut workspace = Workspace::new("Blue");
        workspace.file_path = Some("/show/Practice.inkue".into());
        let active_list = workspace.active_cue_list_id;
        let mut standby = WaitCue::new();
        standby.set_number(Some("101".into()));
        let standby_id = standby.id();
        let active = workspace.active_cue_list_mut().unwrap();
        active.auto_renumber = false;
        active.push(Box::new(standby));
        active.set_playhead(Some(standby_id)).unwrap();

        let mut offstage = CueList::new("Offstage");
        offstage.auto_renumber = false;
        let mut cue = WaitCue::new();
        cue.set_number(Some("202.1".into()));
        cue.set_continue_mode(ContinueMode::DoNotContinue);
        let cue_id = cue.id();
        offstage.push(Box::new(cue));
        let mut child = WaitCue::new();
        child.set_number(Some("202.2".into()));
        let mut group = GroupCue::new();
        group.children.push(Box::new(child));
        offstage.push(Box::new(group));
        workspace.cue_lists.push(offstage);
        let resolve = |address: &str, workspace: &Workspace| {
            let OscAction::AddressedCue { workspace: scope, number, action } = resolve_action(address, &[]) else {
                return None;
            };
            resolve_network_target(workspace, scope, number).map(|(index, id)| (index, id, action))
        };

        let uuid = workspace.instance_id();
        for (verb, expected) in [
            ("start", NetworkCueAction::Start),
            ("stop", NetworkCueAction::Stop),
            ("reset", NetworkCueAction::Reset),
            ("pause", NetworkCueAction::SetPause(NetworkPauseAction::Pause)),
            ("resume", NetworkCueAction::SetPause(NetworkPauseAction::Resume)),
        ] {
            for selector in ["Blue", "Practice.inkue", &uuid.to_string()] {
                let address = format!("/workspace/{selector}/cue/202.1/{verb}");
                let text = parse_text_message(address.as_bytes()).unwrap();
                assert_eq!(resolve(&text.addr, &workspace), Some((1, cue_id, expected.clone())));
                assert!(matches!(resolve_action(&address, &[rosc::OscType::Int(1)]), OscAction::Unmatched));
            }
        }
        for address in ["/cue/202.1/start", "/workspace/blue/cue/202.1/start",
            "/workspace/Other/cue/202.1/start", "/workspace/Blue/cue/202.2/start",
            "/workspace/Blue/cue/*/start", "/workspace/Blue/cue/202.1/go"]
        {
            assert_eq!(resolve(address, &workspace), None, "{address}");
        }
        assert_eq!(resolve(&format!("/workspace/{}/cue/202.1/start", Uuid::new_v4()), &workspace), None);

        let (index, id, _) = resolve("/workspace/Blue/cue/202.1/start", &workspace).unwrap();
        let (context, _, _) = crate::test_support::recording_context();
        let mut transport = Transport::new(context);
        assert_eq!(transport.start_independent(&mut workspace.cue_lists[index], id).unwrap().triggered, [id]);
        assert_eq!(workspace.active_cue_list_id, active_list);
        assert_eq!(workspace.active_cue_list().unwrap().playhead_cue_id, Some(standby_id));
        transport.stop_cue(&mut workspace.cue_lists[index], &id).unwrap();
        assert!(!workspace.cue_lists[index].get(&id).unwrap().is_running());

        let mut duplicate = WaitCue::new();
        duplicate.set_number(Some("202.1".into()));
        workspace.active_cue_list_mut().unwrap().push(Box::new(duplicate));
        assert_eq!(resolve("/workspace/Blue/cue/202.1/start", &workspace), None);
    }

    #[test]
    fn qlab_workspace_transport_requires_an_allowlist_and_exact_workspace() {
        use NetworkWorkspaceAction::*;
        let workspace = Workspace::new("Blue");
        for (path, expected) in [
            ("/go", Go),
            ("/stop", Stop),
            ("/panic", Stop),
            ("/hardStop", HardStop),
            ("/pause", SetPaused(true)),
            ("/resume", SetPaused(false)),
            ("/reset", Reset),
            ("/playhead/none", ClearStandby),
            ("/playhead/next", Step { forward: true, sequence: false }),
            ("/playbackPosition/previous", Step { forward: false, sequence: false }),
            ("/playbackPosition/previousSequence", Step { forward: false, sequence: true }),
        ] {
            let addr = format!("/workspace/Blue{path}");
            assert!(matches!(authorized_action(&addr, &[], false), OscAction::Unmatched));
            assert!(matches!(authorized_action(path, &[], false), OscAction::Unmatched));
            assert!(matches!(authorized_action(&addr, &[rosc::OscType::Int(1)], true), OscAction::Unmatched));
            let message = parse_text_message(addr.as_bytes()).unwrap();
            assert!(matches!(authorized_action(&message.addr, &message.args, true),
                OscAction::WorkspaceTransport { workspace: Some(selector), action }
                    if action == expected && selector.matches(&workspace)));
            assert!(matches!(authorized_action(path, &[], true),
                OscAction::WorkspaceTransport { workspace: None, action } if action == expected));
        }
        assert!(matches!(authorized_action("/workspace/blue/reset", &[], true),
            OscAction::WorkspaceTransport { workspace: Some(selector), .. } if !selector.matches(&workspace)));
        assert!(matches!(authorized_action("/cue/12/reset", &[], true),
            OscAction::AddressedCue { action: NetworkCueAction::Reset, number: "12", .. }));
        assert!(matches!(authorized_action("/cue/12/reset", &[], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/workspace/*/reset", &[], true), OscAction::Unmatched));
    }

    #[test]
    fn qlab_go_to_cue_number_maps_to_a_gated_workspace_transport_and_parks_then_goes() {
        use crate::cue::wait_cue::WaitCue;
        use crate::show::{cue_list::CueList, transport::Transport};

        assert!(matches!(authorized_action("/go/202.1", &[], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/go/202.1", &[rosc::OscType::Int(1)], true), OscAction::Unmatched));
        assert!(matches!(authorized_action("/go/*", &[], true), OscAction::Unmatched));
        assert!(matches!(authorized_action("/go/", &[], true), OscAction::Unmatched));
        assert!(matches!(authorized_action("/go/202.1", &[], true),
            OscAction::WorkspaceTransport { workspace: None, action } if action == NetworkWorkspaceAction::GoToCue("202.1")));
        let scoped = "/workspace/Blue/go/202.1";
        let workspace = Workspace::new("Blue");
        assert!(matches!(authorized_action(scoped, &[], false), OscAction::Unmatched));
        assert!(matches!(authorized_action(scoped, &[], true),
            OscAction::WorkspaceTransport { workspace: Some(selector), action: NetworkWorkspaceAction::GoToCue("202.1") }
            if selector.matches(&workspace)));
        assert!(parse_text_message(b"/go/202.1").is_some());
        assert!(parse_text_message(b"/go/*").is_none());

        let mut workspace = Workspace::new("Blue");
        let active_list = workspace.active_cue_list_id;
        let mut first = WaitCue::new();
        first.set_number(Some("101".into()));
        let first_id = first.id();
        let mut second = WaitCue::new();
        second.set_number(Some("202.1".into()));
        let second_id = second.id();
        let active = workspace.active_cue_list_mut().unwrap();
        active.auto_renumber = false;
        active.push(Box::new(first));
        active.set_playhead(Some(first_id)).unwrap();
        active.push(Box::new(second));

        let target = crate::commands::transport_cmds::go_to_cue_target;
        assert_eq!(target(&workspace, "202.1"), Some((active_list, second_id)));
        assert_eq!(target(&workspace, "nope"), None);

        // A duplicate number in another list loses to the active list's match.
        let mut other = CueList::new("Other");
        other.auto_renumber = false;
        let mut duplicate = WaitCue::new();
        duplicate.set_number(Some("202.1".into()));
        other.push(Box::new(duplicate));
        workspace.cue_lists.push(other);
        assert_eq!(target(&workspace, "202.1"), Some((active_list, second_id)));

        // A number unique to a non-active list switches the active list.
        let mut third = WaitCue::new();
        third.set_number(Some("303".into()));
        let third_id = third.id();
        let other_id = workspace.cue_lists[1].id;
        workspace.cue_lists[1].push(Box::new(third));
        assert_eq!(target(&workspace, "303"), Some((other_id, third_id)));

        // Park the playhead on the numbered cue, then GO triggers it.
        let (context, _, _) = crate::test_support::recording_context();
        let (list_id, id) = target(&workspace, "202.1").unwrap();
        workspace.switch_active_list(list_id);
        let list = workspace.cue_lists.iter_mut().find(|list| list.id == list_id).unwrap();
        list.set_playhead(Some(id)).unwrap();
        let mut transport = Transport::new(context);
        let result = transport.go(list).unwrap();
        assert_eq!(result.triggered, [second_id]);
    }

    #[test]
    fn qlab_playhead_selected_and_cue_id_identifiers_resolve_to_the_target_cue() {
        use crate::cue::{group_cue::GroupCue, wait_cue::WaitCue};

        let mut workspace = Workspace::new("Blue");
        let mut standby = WaitCue::new();
        standby.set_number(Some("101".into()));
        let standby_id = standby.id();
        let mut child = WaitCue::new();
        child.set_number(Some("202.1".into()));
        let child_id = child.id();
        let mut group = GroupCue::new();
        group.children.push(Box::new(child));
        let active = workspace.active_cue_list_mut().unwrap();
        active.auto_renumber = false;
        active.push(Box::new(standby));
        active.set_playhead(Some(standby_id)).unwrap();
        active.push(Box::new(group));

        fn resolve<'a>(address: &'a str, workspace: &Workspace) -> Option<(usize, CueId, &'a str, NetworkCueAction)> {
            let OscAction::AddressedCue { workspace: scope, number, action } = resolve_action(address, &[]) else {
                return None;
            };
            resolve_network_target(workspace, scope, number).map(|(index, id)| (index, id, number, action))
        }

        for (selector, verb, expected) in [
            ("playhead", "stop", NetworkCueAction::Stop),
            ("selected", "start", NetworkCueAction::Start),
            ("selected", "pause", NetworkCueAction::SetPause(NetworkPauseAction::Pause)),
        ] {
            let address = format!("/cue/{selector}/{verb}");
            let (index, id, number, action) = resolve(&address, &workspace).unwrap();
            assert_eq!((index, id), (0, standby_id));
            assert_eq!(number, selector);
            assert_eq!(action, expected);
            let scoped = format!("/workspace/Blue/cue/{selector}/{verb}");
            assert!(matches!(authorized_action(&scoped, &[], false), OscAction::Unmatched));
            let OscAction::AddressedCue { workspace: Some(scope), number, action } = authorized_action(&scoped, &[], true) else {
                panic!("{scoped}");
            };
            assert_eq!((number, action), (selector, expected));
            assert_eq!(
                resolve_network_target(&workspace, Some(scope), number).map(|(index, id)| (index, id)),
                Some((0, standby_id))
            );
        }
        assert!(parse_text_message(b"/cue/selected/stop").is_some());
        assert!(parse_text_message(b"/cue/playhead/start").is_some());

        // `/cue_id/{uuid}` finds cues by unique ID, but only top-level cues are controllable.
        let nested = format!("/cue_id/{child_id}/pause");
        let OscAction::AddressedCue { action, .. } = resolve_action(&nested, &[]) else { panic!() };
        assert_eq!(action, NetworkCueAction::SetPause(NetworkPauseAction::Pause));
        assert_eq!(resolve(&nested, &workspace), None, "nested cue IDs are not directly controllable");
        let top = format!("/cue_id/{standby_id}/stop");
        assert_eq!(resolve(&top, &workspace).map(|(index, id, _, _)| (index, id)), Some((0, standby_id)));
        assert!(parse_text_message(top.as_bytes()).is_some());

        for address in [
            "/cue/playhead/go", "/cue/selected/seek", "/cue/playhead/", "/cue/all/stop", "/cue/*/start",
            "/cue_id/not-a-uuid/stop", "/cue_id/202.1/stop",
        ] {
            assert!(matches!(resolve_action(address, &[]), OscAction::Unmatched), "{address}");
        }
        assert!(matches!(resolve_action("/cue/playhead/stop", &[rosc::OscType::Int(1)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/cue/selected/reset", &[]),
            OscAction::AddressedCue { action: NetworkCueAction::Reset, .. }));
        assert!(matches!(authorized_action("/cue/selected/reset", &[], true),
            OscAction::AddressedCue { action: NetworkCueAction::Reset, number: "selected", .. }));

        // No playhead parked: the reserved identifiers match nothing.
        workspace.active_cue_list_mut().unwrap().set_playhead(None).unwrap();
        assert_eq!(resolve("/cue/playhead/stop", &workspace), None);
        assert_eq!(resolve("/cue/selected/start", &workspace), None);
    }

    #[test]
    fn qlab_override_messages_resolve_are_gated_and_toggle() {
        use crate::engine::overrides::GlobalOverrides;
        assert!(matches!(resolve_action("/overrides/toggleDmxOutput", &[]),
            OscAction::SetOverride { key: "dmx_output", value: None }));
        assert!(matches!(resolve_action("/overrides/dmxOutputEnabled", &[rosc::OscType::Int(0)]),
            OscAction::SetOverride { key: "dmx_output", value: Some(false) }));
        assert!(matches!(resolve_action("/overrides/midiInputEnabled", &[rosc::OscType::String("no".into())]),
            OscAction::SetOverride { key: "midi_input", value: Some(false) }));
        assert!(matches!(resolve_action("/overrides/midiInputEnabled", &[rosc::OscType::String("yep".into())]),
            OscAction::SetOverride { key: "midi_input", value: Some(true) }));
        assert!(matches!(resolve_action("/overrides/networkExternalInputEnabled/toggle", &[]),
            OscAction::SetOverride { key: "network_external_input", value: None }));
        assert!(matches!(resolve_action("/overrides/timecodeInputEnabled", &[rosc::OscType::String("toggle".into())]),
            OscAction::SetOverride { key: "timecode_input", value: None }));
        assert!(matches!(resolve_action("/overrides/toggleMidiOutput", &[rosc::OscType::Int(1)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/overrides/toggleBogus", &[]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/overrides/dmxOutputEnabled", &[rosc::OscType::String("maybe".into())]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/overrides/dmxOutputEnabled/extra", &[]), OscAction::Unmatched));
        // Control-level: an allowlisted sender only.
        assert!(matches!(authorized_action("/overrides/toggleDmxOutput", &[], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/overrides/toggleDmxOutput", &[], true), OscAction::SetOverride { .. }));
        assert!(parse_text_message(b"/overrides/toggleMidiInput").is_some());
        assert!(parse_text_message(b"/overrides/dmxOutputEnabled 1").is_none());

        // Toggle semantics on a local instance: None flips the current value,
        // an explicit bool sets it.
        let local = GlobalOverrides::new();
        let OscAction::SetOverride { key, value } = resolve_action("/overrides/toggleMscOutput", &[]) else { panic!() };
        let enabled = value.unwrap_or_else(|| !local.get(key).unwrap_or(true));
        local.set(key, enabled).unwrap();
        assert_eq!(local.get("msc_output"), Some(false));
        let OscAction::SetOverride { key, value } = resolve_action("/overrides/mscOutputEnabled", &[rosc::OscType::Int(1)]) else { panic!() };
        let enabled = value.unwrap_or_else(|| !local.get(key).unwrap_or(true));
        local.set(key, enabled).unwrap();
        assert_eq!(local.get("msc_output"), Some(true));
    }

    #[test]
    fn qlab_cue_property_writes_parse_validate_and_require_the_allowlist() {
        use crate::cue::types::ContinueMode;
        let d = |v: f64| rosc::OscType::Double(v);
        let s = |v: &str| rosc::OscType::String(v.to_owned());

        assert!(matches!(resolve_action("/cue/5/preWait", &[d(2.5)]),
            OscAction::AddressedCue { number: "5", action: NetworkCueAction::SetPreWait(v), .. } if v == 2.5));
        assert!(matches!(resolve_action("/cue/5/postWait", &[d(0.0)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetPostWait(0.0), .. }));
        assert!(matches!(resolve_action("/cue/selected/armed", &[rosc::OscType::Int(0)]),
            OscAction::AddressedCue { number: "selected", action: NetworkCueAction::SetArmed(false), .. }));
        assert!(matches!(resolve_action("/cue/5/armed", &[s("yes")]),
            OscAction::AddressedCue { action: NetworkCueAction::SetArmed(true), .. }));
        assert!(matches!(resolve_action("/cue/5/qName", &[s("LX 5")]),
            OscAction::AddressedCue { action: NetworkCueAction::SetQName(ref name), .. } if name == "LX 5"));
        assert!(matches!(resolve_action("/cue/5/continueMode", &[s("auto_follow")]),
            OscAction::AddressedCue { action: NetworkCueAction::SetContinueMode(ContinueMode::AutoFollow), .. }));
        let uuid = uuid::Uuid::new_v4();
        assert!(matches!(resolve_action(&format!("/cue_id/{uuid}/preWait"), &[d(1.0)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetPreWait(1.0), .. }));

        // Bad or missing arguments stay unmatched, and so do writes on verbs that take none.
        for (address, args) in [
            ("/cue/5/preWait", &[][..]),
            ("/cue/5/preWait", &[d(-1.0)][..]),
            ("/cue/5/preWait", &[d(f64::NAN)][..]),
            ("/cue/5/preWait", &[d(1.0), d(2.0)][..]),
            ("/cue/5/start", &[d(1.0)][..]),
            ("/cue/5/armed", &[s("maybe")][..]),
            ("/cue/5/qName", &[s("")][..]),
            ("/cue/5/continueMode", &[s("teleport")][..]),
            ("/cue/5/level", &[d(0.5)][..]),
        ] {
            assert!(matches!(resolve_action(address, args), OscAction::Unmatched), "{address} {args:?}");
        }

        // Writes are control-level: they need an explicitly allowlisted sender,
        // even unscoped, while transport verbs on the same cue do not.
        assert!(matches!(authorized_action("/cue/5/preWait", &[d(1.0)], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/cue/5/preWait", &[d(1.0)], true),
            OscAction::AddressedCue { action, .. } if action.is_write()));
        assert!(matches!(authorized_action("/cue/5/stop", &[], false),
            OscAction::AddressedCue { action: NetworkCueAction::Stop, .. }));

        // Plain text takes one numeric or string argument for cue writes only.
        let parsed = parse_text_message(b"/cue/5/preWait 2.5").unwrap();
        assert_eq!(parsed.args, vec![d(2.5)]);
        let parsed = parse_text_message(b"/cue/5/qName LX5").unwrap();
        assert_eq!(parsed.args, vec![s("LX5")]);
        assert!(parse_text_message(b"/cue/5/qName LX 5").is_none(), "plain-text arguments cannot contain spaces");
        assert!(parse_text_message(b"/cue/5/stop 1").is_none());
    }

    #[test]
    fn qlab_wildcard_cue_addresses_match_numbers_and_apply_safely() {
        use crate::cue::{group_cue::GroupCue, wait_cue::WaitCue};

        assert!(glob_match("*", "anything"));
        assert!(glob_match("*1", "101"));
        assert!(glob_match("1*", "1"));
        assert!(glob_match("ham*", "hamlet"));
        assert!(glob_match("h*t", "hamlet"));
        assert!(glob_match("h*t", "ht"));
        assert!(!glob_match("h*t", "ham"));
        assert!(!glob_match("*1", "11a"));
        assert!(glob_match("", ""));
        assert!(glob_match("*", ""));

        let mut workspace = Workspace::new("Blue");
        let mut a = WaitCue::new();
        a.set_number(Some("101".into()));
        let a_id = a.id();
        let mut b = WaitCue::new();
        b.set_number(Some("201".into()));
        let b_id = b.id();
        let mut c = WaitCue::new();
        c.set_number(Some("202".into()));
        let c_id = c.id();
        let mut child = WaitCue::new();
        child.set_number(Some("101".into()));
        let mut group = GroupCue::new();
        group.children.push(Box::new(child));
        let list = workspace.active_cue_list_mut().unwrap();
        list.auto_renumber = false;
        list.push(Box::new(a));
        list.push(Box::new(b));
        list.push(Box::new(c));
        list.push(Box::new(group));

        // Transport verbs on patterns resolve to AddressedCuePattern.
        for (address, expected) in [
            ("/cue/*/stop", NetworkCueAction::Stop),
            ("/cue/2*/pause", NetworkCueAction::SetPause(NetworkPauseAction::Pause)),
            ("/cue/*/resume", NetworkCueAction::SetPause(NetworkPauseAction::Resume)),
            ("/cue/*/reset", NetworkCueAction::Reset),
        ] {
            assert!(matches!(resolve_action(address, &[]),
                OscAction::AddressedCuePattern { workspace: None, pattern, action } if action == expected && pattern == address.strip_prefix("/cue/").unwrap().split('/').next().unwrap()),
                "{address}");
        }
        // Never start/select every cue; writes need their one argument; args on verbs stay out.
        for address in ["/cue/*/start", "/cue/*/select", "/cue/*/go", "/cue/*/stop 1"] {
            let args: &[rosc::OscType] = if address.ends_with(" 1") { &[rosc::OscType::Int(1)] } else { &[] };
            assert!(matches!(resolve_action(address.trim_end_matches(" 1"), args), OscAction::Unmatched), "{address}");
        }
        assert!(matches!(resolve_action("/cue/*/armed", &[rosc::OscType::Int(0)]),
            OscAction::AddressedCuePattern { action, .. } if action.is_write()));
        assert!(matches!(resolve_action("/cue/*/armed", &[]), OscAction::Unmatched));

        // Resolution: `*` hits all three top-level cues, never the nested duplicate.
        let all = resolve_network_pattern(&workspace, None, "*");
        assert_eq!(all.len(), 3);
        assert!(all.contains(&(0, a_id)) && all.contains(&(0, b_id)) && all.contains(&(0, c_id)));
        let twos = resolve_network_pattern(&workspace, None, "2*");
        assert_eq!(twos, vec![(0, b_id), (0, c_id)]);
        assert!(resolve_network_pattern(&workspace, None, "9*").is_empty());
        // Scoped pattern searches every list's top level.
        let scoped = resolve_network_pattern(&workspace, Some(WorkspaceSelector::Name("Blue")), "*");
        assert_eq!(scoped.len(), 3);
        assert!(resolve_network_pattern(&workspace, Some(WorkspaceSelector::Name("Other")), "*").is_empty());

        // Gating mirrors single-cue rules: unscoped stop is open, reset and writes need the allowlist.
        assert!(matches!(authorized_action("/cue/*/stop", &[], false),
            OscAction::AddressedCuePattern { action: NetworkCueAction::Stop, .. }));
        assert!(matches!(authorized_action("/cue/*/reset", &[], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/cue/*/armed", &[rosc::OscType::Int(1)], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/workspace/Blue/cue/*/stop", &[], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/workspace/Blue/cue/*/stop", &[], true),
            OscAction::AddressedCuePattern { workspace: Some(_), .. }));
        assert!(parse_text_message(b"/cue/*/stop").is_some());
        assert!(parse_text_message(b"/cue/*/start").is_none());
    }

    #[test]
    fn qlab_thump_is_acknowledged_without_activity() {
        assert!(matches!(resolve_action("/thump", &[]), OscAction::Heartbeat));
        assert!(matches!(authorized_action("/thump", &[], false), OscAction::Heartbeat));
        assert!(matches!(authorized_action("/workspace/Blue/thump", &[], false), OscAction::Heartbeat));
        assert!(matches!(resolve_action("/thump", &[rosc::OscType::Int(1)]), OscAction::Unmatched));
        assert!(parse_text_message(b"/thump\r\n").is_some());
    }

    #[test]
    fn scoped_query_counts_nested_rows_before_returning_an_exact_match() {
        use crate::cue::{group_cue::GroupCue, memo_cue::MemoCue};
        let mut workspace = Workspace::new("Large rehearsal");
        let mut child = MemoCue::new();
        child.set_number(Some("1".into()));
        let mut group = GroupCue::new();
        group.children.extend((0..MAX_SCOPED_ROWS - 2)
            .map(|_| Box::new(MemoCue::new()) as Box<dyn Cue>));
        group.children.push(Box::new(child));
        let group_id = group.id();
        let list = workspace.active_cue_list_mut().unwrap();
        list.push(Box::new(group));
        let address = format!("/workspace/{}/cue/1/number", workspace.instance_id());
        let message = query_message(&address);
        let query = CueQuery::parse(&message).unwrap();
        assert_eq!(query.read(&workspace), Some("1".into()));
        let list = workspace.active_cue_list_mut().unwrap();
        list.get_mut_recursive(&group_id).unwrap().child_cues_mut().unwrap().push(Box::new(MemoCue::new()));
        assert_eq!(query.read(&workspace), None);
    }

    #[test]
    fn binary_queries_require_explicit_allowlisting_and_reply_to_each_requesting_socket() {
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let protocol = Protocol::Osc;
        let mut config = OscReceiveConfig { enabled: true, port: 0, ..OscReceiveConfig::default() };
        let instance = uuid::Uuid::new_v4();
        let listener = Listener::spawn(
            protocol, protocol.settings(&config), || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            |_, _| {},
            move |query| query.encode_reply(instance, Some(false.into())),
        );
        let OscListenerStatus::Listening { address } = statuses.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected an OSC socket");
        };
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let second = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        for peer in [&sender, &second] {
            peer.set_read_timeout(Some(Duration::from_millis(220))).unwrap();
        }
        let query = query_packet("/cue/101/isRunning");
        let mut response = [0; 1024];
        sender.send_to(&query, address).unwrap();
        assert!(sender.recv_from(&mut response).is_err(), "an empty allowlist cannot query");
        config.allowed_ips = vec!["192.0.2.3".into()];
        listener.reconfigure(protocol.settings(&config));
        sender.send_to(&query, address).unwrap();
        assert!(sender.recv_from(&mut response).is_err(), "an unlisted peer cannot query");
        config.allowed_ips = vec!["127.0.0.1".into()];
        listener.reconfigure(protocol.settings(&config));
        // Wait beyond one receive timeout so the worker's next iteration has applied the allowlist.
        std::thread::sleep(Duration::from_millis(130));
        sender.send_to(&query, address).unwrap();
        second.send_to(&query, address).unwrap();
        for peer in [&sender, &second] {
            let (length, origin) = peer.recv_from(&mut response).unwrap();
            assert_eq!(origin, address);
            let (remaining, rosc::OscPacket::Message(message)) = rosc::decoder::decode_udp(&response[..length]).unwrap() else {
                panic!("expected a single OSC reply");
            };
            assert!(remaining.is_empty());
            assert_eq!(message.addr, "/reply/cue/101/isRunning");
            let [rosc::OscType::String(json)] = message.args.as_slice() else { panic!("expected JSON data"); };
            assert_eq!(serde_json::from_str::<serde_json::Value>(json).unwrap(), serde_json::json!({
                "workspace_id": instance, "address": "/cue/101/isRunning", "status": "ok", "data": false
            }));
        }

        let mut malformed_tags = query.clone();
        let tag_index = malformed_tags.len() - 4;
        malformed_tags[tag_index] = b'!';
        sender.send_to(&malformed_tags, address).unwrap();
        assert!(sender.recv_from(&mut response).is_err(), "an invalid OSC type tag cannot query");
        let mut trailing = query;
        trailing.extend_from_slice(&[0; 4]);
        sender.send_to(&trailing, address).unwrap();
        assert!(sender.recv_from(&mut response).is_err(), "a trailing payload cannot query");
        let bundle = rosc::encoder::encode(&rosc::OscPacket::Bundle(rosc::OscBundle {
            timetag: (0, 1).into(), content: vec![rosc::OscPacket::Message(query_message("/cue/101/isRunning"))],
        })).unwrap();
        sender.send_to(&bundle, address).unwrap();
        assert!(sender.recv_from(&mut response).is_err(), "bundled queries cannot reply");
        listener.stop();
    }

    #[test]
    fn listener_reports_a_real_conflict_and_retries_the_same_endpoint() {
        let occupied = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = occupied.local_addr().unwrap();
        let config = OscReceiveConfig {
            enabled: true,
            port: address.port(),
            allowed_ips: vec![Ipv4Addr::LOCALHOST.to_string()],
            ..OscReceiveConfig::default()
        };
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let (packet_tx, packets) = crossbeam_channel::unbounded();
        let server = Listener::spawn(
            Protocol::Osc,
            Protocol::Osc.settings(&config),
            || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            move |packet, explicitly_allowed| { let _ = packet_tx.send((packet.clone(), explicitly_allowed)); },
            |_| None,
        );
        let timeout = Duration::from_secs(2);
        assert!(matches!(
            statuses.recv_timeout(timeout).unwrap(),
            OscListenerStatus::BindFailed { address: attempted, error } if attempted == address && !error.is_empty()
        ));

        drop(occupied);
        server.retry().unwrap();
        let listening = OscListenerStatus::Listening { address };
        assert_eq!(statuses.recv_timeout(timeout).unwrap(), listening);
        assert_eq!(server.status().unwrap(), listening);

        // Retry is harmless when the listener is already healthy.
        server.retry().unwrap();
        let packet = rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/inkue/cue/1/select".to_string(),
            args: vec![],
        });
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        sender.send_to(&rosc::encoder::encode(&packet).unwrap(), address).unwrap();
        assert_eq!(packets.recv_timeout(timeout).unwrap(), (packet, true));
        assert!(statuses.try_recv().is_err());
    }

    #[test]
    fn retry_cannot_enable_osc_and_listening_reports_the_assigned_port() {
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let config = OscReceiveConfig { port: 0, ..OscReceiveConfig::default() };
        let server = Listener::spawn(
            Protocol::Osc,
            Protocol::Osc.settings(&config),
            || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            |_, _| {},
            |_| None,
        );
        let timeout = Duration::from_secs(2);
        assert_eq!(statuses.recv_timeout(timeout).unwrap(), OscListenerStatus::Disabled);
        server.retry().unwrap();
        assert!(statuses.recv_timeout(Duration::from_millis(150)).is_err());
        assert_eq!(server.status().unwrap(), OscListenerStatus::Disabled);

        server.reconfigure(Protocol::Osc.settings(&OscReceiveConfig { enabled: true, ..config }));
        let OscListenerStatus::Listening { address } = statuses.recv_timeout(timeout).unwrap() else {
            panic!("expected an active loopback socket");
        };
        assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(address.port(), 0);
        assert!(UdpSocket::bind(address).is_err());
    }

    #[test]
    fn ip_allowlist_empty_accepts_all() {
        assert!(is_allowed(&[], "192.168.1.1"));
        assert!(is_allowed(&[], "10.0.0.1"));
    }

    #[test]
    fn ip_allowlist_filters_correctly() {
        let allowed = vec!["192.168.1.100".to_string()];
        assert!(is_allowed(&allowed, "192.168.1.100"));
        assert!(!is_allowed(&allowed, "192.168.1.101"));
        assert!(!is_allowed(&allowed, "127.0.0.1"));
    }

    #[test]
    fn legacy_settings_leave_text_receive_off() {
        let old: OscReceiveConfig = serde_json::from_str(r#"{"enabled":true,"port":53111}"#).unwrap();
        assert!(old.enabled);
        assert_eq!(old.port, 53111);
        assert!(!old.plain_text_enabled);
        assert_eq!(old.plain_text_port, 53535);
    }

    #[test]
    fn text_routes_through_normal_command_resolution() {
        for (input, command) in [
            ("/go\r\n", "go"),
            ("/stop", "stop_all"),
            ("/inkue/cue/102/seek/relative\t-1.25", "cue_seek"),
            ("/inkue/cue/102/seek/percent 0.5", "cue_seek"),
        ] {
            let parsed = parse_text_message(input.as_bytes()).unwrap();
            let payload = command_payload(resolve_action(&parsed.addr, &parsed.args));
            assert_eq!(payload["command"], command);
        }
    }

    #[test]
    fn text_rejects_invalid_oversize_ambiguous_or_multiple_commands() {
        for input in [
            "", "/go 2", "/inkue/go\0", "/inkue/go\n/inkue/stop", "/inkue/go /inkue/stop",
            "/inkue/cue//go", "/inkue/cue/*/go", "/cue/all/stop", "/cue/102/start 1",
            "/cue/*/start", "/cue/*/stop 1", "/inkue/cue/102/seek/relative/extra 1", "/inkue/cue/102/seek",
            "/playhead/active", "/playbackPosition/previous/extra", "/playhead/1.5 1", "/playhead/*", "/playhead/1.5/extra",
            "/inkue/cue/102/seek NaN", "/inkue/cue/102/seek/percent 1.2", "/unknown",
        ] {
            assert!(parse_text_message(input.as_bytes()).is_none(), "{input:?}");
        }
        assert!(parse_text_message(b"/inkue/cue/\xff/go").is_none());
        let mut oversize = b"/inkue/go".to_vec();
        oversize.resize(1025, b' ');
        assert!(parse_text_message(&oversize).is_none());
    }

    #[test]
    fn text_udp_requires_shared_allowlist_even_when_osc_allows_empty() {
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let (packet_tx, packets) = crossbeam_channel::unbounded();
        let protocol = Protocol::PlainText;
        let mut config = OscReceiveConfig { plain_text_enabled: true, plain_text_port: 0, ..OscReceiveConfig::default() };
        let listener = Listener::spawn(
            protocol,
            protocol.settings(&config),
            || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            move |packet, _| { let _ = packet_tx.send(packet.clone()); },
            |_| None,
        );
        let timeout = Duration::from_secs(2);
        let OscListenerStatus::Listening { address } = statuses.recv_timeout(timeout).unwrap() else {
            panic!("expected a plain text socket");
        };
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let send = || sender.send_to(b"/inkue/cue/102/go", address).unwrap();
        send();
        let poll = Duration::from_millis(180);
        assert!(packets.recv_timeout(poll).is_err());
        config.allowed_ips = vec!["192.0.2.3".into()];
        listener.reconfigure(protocol.settings(&config));
        send();
        assert!(packets.recv_timeout(poll).is_err());
        config.allowed_ips = vec!["127.0.0.1".into()];
        listener.reconfigure(protocol.settings(&config));
        let received = (0..6).find_map(|_| {
            send();
            packets.recv_timeout(poll).ok()
        });
        assert_eq!(received, Some(rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/inkue/cue/102/go".into(), args: vec![],
        })));
        assert_eq!(listener.status().unwrap(), OscListenerStatus::Listening { address });

        config.plain_text_enabled = false;
        listener.reconfigure(protocol.settings(&config));
        assert_eq!(statuses.recv_timeout(timeout).unwrap(), OscListenerStatus::Disabled);
        sender.send_to(b"/inkue/go", address).unwrap();
        assert!(packets.recv_timeout(poll).is_err());
    }

    #[test]
    fn binary_osc_and_plain_text_share_exact_numbered_native_actions() {
        use NetworkCueAction::{Go, Select, SetPause, Start, Stop};
        use NetworkPauseAction::{Pause, Resume};

        for (address, cue_number, expected) in [
            ("/inkue/cue/1.5/go", "1.5", Go),
            ("/inkue/cue/Intro/start", "Intro", Start),
            ("/cue/102/start", "102", Start),
            ("/inkue/cue/Intro/select", "Intro", Select),
            ("/playhead/1.5", "1.5", Select),
            ("/playbackPosition/Intro", "Intro", Select),
            ("/inkue/cue/3/stop", "3", Stop),
            ("/cue/1.5/stop", "1.5", Stop),
            ("/inkue/cue/3/pause", "3", SetPause(Pause)),
            ("/inkue/cue/3/resume", "3", SetPause(Resume)),
            ("/cue/1.5/pause", "1.5", SetPause(Pause)),
            ("/cue/1.5/resume", "1.5", SetPause(Resume)),
            ("/cue/selected/pause", "selected", SetPause(Pause)),
            ("/cue/playhead/stop", "playhead", Stop),
        ] {
            let parsed = parse_text_message(address.as_bytes()).unwrap();
            for action in [resolve_action(address, &[]), resolve_action(&parsed.addr, &parsed.args)] {
                assert!(matches!(action, OscAction::AddressedCue { workspace: None, number, action }
                    if number == cue_number && action == expected), "{address}");
            }
        }
        for invalid in ["/inkue/cue//start", "/cue/all/stop", "/playhead/next/extra", "/cue/*/start",
            "/playhead/1.5/extra", "/select/1.5", "/workspace/show/playhead/1.5", "/cue/1.5/go",
            "/cue/all/resume", "/cue/*/resume 1", "/cue/3/resume/extra", "/cue/selected/go"]
        {
            assert!(!is_matched(invalid, &[]), "{invalid}");
        }
        for invalid_with_args in ["/inkue/cue/11/start", "/inkue/cue/11/select", "/inkue/cue/11/stop", "/cue/11/stop", "/playhead/11",
            "/inkue/cue/11/pause", "/inkue/cue/11/resume", "/cue/11/pause", "/cue/11/resume"] {
            assert!(!is_matched(invalid_with_args, &[rosc::OscType::Int(1)]), "{invalid_with_args}");
        }
    }

    #[test]
    fn list_pause_controls_never_dispatch_a_frontend_command() {
        use NetworkPauseAction::{Pause, Resume, Toggle};

        for (address, expected) in [("/inkue/pause", Pause), ("/inkue/resume", Resume), ("/inkue/pause_toggle", Toggle)] {
            let parsed = parse_text_message(address.as_bytes()).unwrap();
            assert!(matches!(resolve_action(&parsed.addr, &parsed.args), OscAction::ListPause(action) if action == expected));
            // Legacy binary controllers may send a button value, previously ignored.
            assert!(matches!(resolve_action(address, &[rosc::OscType::Int(1)]), OscAction::ListPause(action) if action == expected));
        }
    }

    fn command_payload(action: OscAction<'_>) -> serde_json::Value {
        let OscAction::Command(payload) = action else { panic!("expected a frontend command"); };
        payload
    }

    #[test]
    fn parse_cue_seek_absolute() {
        let payload = command_payload(parse_cue_address("/inkue/cue/3/seek", &[rosc::OscType::Float(12.5)]));
        assert_eq!(payload["command"], "cue_seek");
        assert_eq!(payload["cue_number"], "3");
        assert_eq!(payload["seek_mode"], "absolute");
        assert_eq!(payload["value"], 12.5);
    }

    #[test]
    fn parse_cue_seek_relative_and_percent() {
        let rel = command_payload(parse_cue_address("/inkue/cue/1.5/seek/relative", &[rosc::OscType::Int(-10)]));
        assert_eq!(rel["seek_mode"], "relative");
        assert_eq!(rel["value"], -10.0);
        assert_eq!(rel["cue_number"], "1.5");

        let pct = command_payload(parse_cue_address("/inkue/cue/Intro/seek/percent", &[rosc::OscType::Double(0.5)]));
        assert_eq!(pct["seek_mode"], "percent");
        assert_eq!(pct["value"], 0.5);
    }

    #[test]
    fn parse_cue_seek_without_value_is_ignored() {
        assert!(!is_matched("/inkue/cue/3/seek", &[rosc::OscType::String("x".into())]));
        assert!(!is_matched("/inkue/cue/3/seek/backwards", &[rosc::OscType::Float(1.0)]));
    }

    fn is_matched(addr: &str, args: &[rosc::OscType]) -> bool {
        !matches!(resolve_action(addr, args), OscAction::Unmatched)
    }

    #[test]
    fn resolve_action_matches_all_command_addresses() {
        for addr in [
            "/inkue/go", "/inkue/stop", "/inkue/hardstop",
            "/inkue/pause", "/inkue/resume", "/inkue/pause_toggle",
            "/inkue/select/next", "/inkue/select/previous",
            "/inkue/cues/request", "/inkue/playhead/request",
            "/inkue/cue/11/go", "/inkue/cue/11/select", "/inkue/cue/11/stop",
        ] {
            assert!(is_matched(addr, &[]), "{addr} should match");
        }
        for addr in [
            "/inkue/cue/11/seek",
            "/inkue/cue/11/seek/relative",
            "/inkue/cue/11/seek/percent",
        ] {
            assert!(is_matched(addr, &[rosc::OscType::Float(1.0)]), "{addr} should match");
        }
    }

    #[test]
    fn resolve_action_flags_unknown_addresses() {
        assert!(!is_matched("/jog_wheel", &[rosc::OscType::Float(1.0)]));
        assert!(!is_matched("/inkue/cue/11/teleport", &[]));
        assert!(!is_matched("/inkue/cue/11/seek", &[])); // seek without a value
        assert!(!is_matched("/inkue/nope", &[]));
    }
}
