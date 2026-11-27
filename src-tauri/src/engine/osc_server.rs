//! Show-control receiver for OSC and optional plain text UDP.
//!
//! Architecture:
//! - One background thread per socket.
//! - `recv_from` with a 100 ms read timeout so config changes take effect quickly.
//! - Native transport handles workspace commands and addressed cue actions.
//! - A few binary OSC queries reply directly to an explicitly allowlisted peer.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use tauri::{Emitter, Manager};
use uuid::Uuid;

use crate::cue::{traits::Cue, types::CueId};
use crate::commands::transport_cmds::{
    address_network_cue_pattern, address_numbered_network_cue, control_network_list_pause,
    control_network_workspace, NetworkCueAction, NetworkPauseAction, NetworkWorkspaceAction,
    VideoProp, VideoPropValue,
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
    osc_tcp: TcpEndpoint,
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

#[derive(Clone)]
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
                move |packet, explicitly_allowed, src| handle_packet(packet, explicitly_allowed, src, &packet_handle),
                move |query, src| handle_query(query, src, &query_handle),
            )
        };
        let tcp_status_handle = app_handle.clone();
        let tcp_packet_handle = app_handle.clone();
        let tcp_query_handle = app_handle.clone();
        let osc_tcp = TcpEndpoint::spawn(
            Protocol::Osc.settings(&config),
            super::net_interface::bind_ip,
            move |status| { let _ = tcp_status_handle.emit("osc-tcp-listener-status", status); },
            move |packet, explicitly_allowed, src| handle_packet(packet, explicitly_allowed, src, &tcp_packet_handle),
            move |query, src| handle_query(query, src, &tcp_query_handle),
        );
        Self { osc: spawn(Protocol::Osc), osc_tcp, plain_text: spawn(Protocol::PlainText) }
    }

    pub fn reconfigure(&self, config: OscReceiveConfig) {
        self.osc.reconfigure(Protocol::Osc.settings(&config));
        self.osc_tcp.reconfigure(Protocol::Osc.settings(&config));
        self.plain_text.reconfigure(Protocol::PlainText.settings(&config));
    }

    /// Status of the OSC TCP accept socket (same port as the UDP listener).
    pub fn tcp_status(&self) -> Result<OscListenerStatus, String> {
        self.osc_tcp.status()
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

    /// Shut down all workers.
    pub fn stop(&self) {
        self.osc.stop();
        self.osc_tcp.stop();
        self.plain_text.stop();
    }
}

impl Listener {
    fn spawn(
        protocol: Protocol,
        config: ListenerConfig,
        bind_ip: impl Fn() -> IpAddr + Send + 'static,
        mut on_status: impl FnMut(&OscListenerStatus) + Send + 'static,
        on_packet: impl FnMut(&rosc::OscPacket, bool, SocketAddr) + Send + 'static,
        on_query: impl FnMut(CueQuery<'_>, SocketAddr) -> Option<Vec<u8>> + Send + 'static,
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
    mut on_packet: impl FnMut(&rosc::OscPacket, bool, SocketAddr),
    mut on_query: impl FnMut(CueQuery<'_>, SocketAddr) -> Option<Vec<u8>>,
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
                    if !crate::engine::overrides::OVERRIDES.network_local_input() && src.ip().is_loopback() {
                        log::debug!("{label}: dropping local packet while the local network input override is off");
                        continue;
                    }
                    let data = &buf[..n];
                    let Some(packet) = protocol.decode(data) else {
                        log::debug!("{label}: ignored malformed or unsupported packet");
                        continue;
                    };
                    let reply = |bytes: &[u8]| { let _ = socket.send_to(bytes, reply_target(src)); };
                    handle_decoded_packet(&mut Dispatch {
                        protocol,
                        allowed_ips: &config.allowed_ips,
                        src,
                        dedup: Some(&mut dedup),
                        reply: &reply,
                        on_packet: &mut on_packet,
                        on_query: &mut on_query,
                        label,
                    }, &packet, data);
                }
                Err(e) if is_timeout(&e) => {}
                Err(e) => log::warn!("{label}: recv error: {e}"),
            }
        }
    }
}

/// Shared per-packet dispatch context, used by both the UDP loop and TCP
/// connections. `reply` sends raw reply packet bytes back to the sender (UDP
/// datagram or SLIP-framed TCP write); `dedup` is `None` on TCP streams.
struct Dispatch<'a> {
    protocol: Protocol,
    allowed_ips: &'a [String],
    src: SocketAddr,
    dedup: Option<&'a mut DedupCache>,
    reply: &'a dyn Fn(&[u8]),
    on_packet: &'a mut dyn FnMut(&rosc::OscPacket, bool, SocketAddr),
    on_query: &'a mut dyn FnMut(CueQuery<'_>, SocketAddr) -> Option<Vec<u8>>,
    label: &'a str,
}

/// Shared dispatch for one decoded OSC packet, used by both the UDP loop and
/// TCP connections.
fn handle_decoded_packet(ctx: &mut Dispatch<'_>, packet: &rosc::OscPacket, data: &[u8]) {
    let protocol = ctx.protocol;
    let allowed_ips = ctx.allowed_ips;
    let src = ctx.src;
    let label = ctx.label;
    if let rosc::OscPacket::Message(message) = packet {
        if let Some(query) = CueQuery::parse(message) {
            // QLab always accepts the app-level queries (/version, /workspaces,
            // /overrides reads, /alwaysReply and /updates status reads);
            // show-data queries need the allowlist.
            if matches!(protocol, Protocol::Osc)
                && query.is_single_datagram(data)
                && (!allowed_ips.is_empty() || query.is_any_access())
            {
                if let Some(bytes) = (ctx.on_query)(query, src) {
                    (ctx.reply)(&bytes);
                }
            }
            // Query results are unicast and can change between identical requests.
            return;
        }
    }
    if let Some(dedup) = &mut ctx.dedup {
        let fp = packet_fingerprint(data);
        if dedup.is_duplicate(fp) {
            log::debug!("{label}: dropped duplicate packet from {src}");
            return;
        }
    }
    crate::engine::osc_updates::touch(src);
    if let rosc::OscPacket::Message(message) = packet {
        if let OscAction::AlwaysReply(on) = resolve_action(&message.addr, &message.args) {
            set_always_reply(src, on);
        }
        touch_always_reply(src);
        // Queries already get a unicast reply above; answer every other
        // message from an alwaysReply client.
        if always_reply_enabled(src) && CueQuery::parse(message).is_none() {
            let matched = !matches!(
                resolve_action(&message.addr, &message.args),
                OscAction::Unmatched
            );
            let json = serde_json::json!({
                "address": message.addr,
                "status": if matched { "ok" } else { "error" },
            });
            if let Ok(bytes) = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
                addr: format!("/reply{}", message.addr),
                args: vec![rosc::OscType::String(json.to_string())],
            })) {
                (ctx.reply)(&bytes);
            }
        }
    }
    (ctx.on_packet)(packet, !allowed_ips.is_empty(), src);
}

// ---------------------------------------------------------------------------
// TCP transport (QLab serves OSC on TCP 53000 alongside UDP, SLIP-framed)
// ---------------------------------------------------------------------------

/// RFC 1055 SLIP encode (OSC 1.1 TCP framing): END-delimited, 0xDB escapes.
pub(crate) fn slip_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 4);
    out.push(0xC0);
    for &byte in data {
        match byte {
            0xC0 => out.extend_from_slice(&[0xDB, 0xDC]),
            0xDB => out.extend_from_slice(&[0xDB, 0xDD]),
            _ => out.push(byte),
        }
    }
    out.push(0xC0);
    out
}

/// Incremental SLIP decoder: returns each complete END-delimited packet payload.
struct SlipDecoder {
    buf: Vec<u8>,
    escaped: bool,
}

impl SlipDecoder {
    fn new() -> Self {
        Self { buf: Vec::new(), escaped: false }
    }

    fn feed(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        for &byte in data {
            match byte {
                0xC0 => {
                    if !self.buf.is_empty() {
                        packets.push(std::mem::take(&mut self.buf));
                    }
                    self.escaped = false;
                }
                0xDB => self.escaped = true,
                _ if self.escaped => {
                    self.buf.push(match byte {
                        0xDC => 0xC0,
                        0xDD => 0xDB,
                        other => other,
                    });
                    self.escaped = false;
                }
                _ => self.buf.push(byte),
            }
        }
        packets
    }
}

type PacketHandler = Arc<dyn Fn(&rosc::OscPacket, bool, SocketAddr) + Send + Sync>;
type QueryHandler = Arc<dyn Fn(CueQuery<'_>, SocketAddr) -> Option<Vec<u8>> + Send + Sync>;

/// Live TCP connections by peer address, so `/update/...` pushes can reach them.
static TCP_CONNECTIONS: Mutex<Option<HashMap<SocketAddr, Arc<Mutex<std::net::TcpStream>>>>> =
    Mutex::new(None);

/// The TCP stream of a connected OSC peer, if it is one.
pub(crate) fn tcp_connection(addr: SocketAddr) -> Option<Arc<Mutex<std::net::TcpStream>>> {
    let guard = TCP_CONNECTIONS.lock().ok()?;
    guard.as_ref()?.get(&addr).cloned()
}

/// The TCP accept endpoint for binary OSC (same port as the UDP listener).
struct TcpEndpoint {
    commands: Sender<ListenerCommand>,
    status: Arc<Mutex<OscListenerStatus>>,
}

impl TcpEndpoint {
    fn spawn(
        config: ListenerConfig,
        bind_ip: impl Fn() -> IpAddr + Send + 'static,
        mut on_status: impl FnMut(&OscListenerStatus) + Send + 'static,
        on_packet: impl Fn(&rosc::OscPacket, bool, SocketAddr) + Send + Sync + 'static,
        on_query: impl Fn(CueQuery<'_>, SocketAddr) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let initial = if config.enabled { OscListenerStatus::Binding } else { OscListenerStatus::Disabled };
        let status = Arc::new(Mutex::new(initial));
        let shared = status.clone();
        let on_packet = Arc::new(on_packet);
        let on_query = Arc::new(on_query);
        std::thread::Builder::new()
            .name("inkue-osc-tcp-receiver".into())
            .spawn(move || tcp_accept_loop(config, rx, bind_ip, move |status| {
                if let Ok(mut slot) = shared.lock() {
                    *slot = status.clone();
                }
                on_status(&status);
            }, on_packet, on_query))
            .expect("Failed to spawn OSC TCP listener thread");
        Self { commands: tx, status }
    }

    fn reconfigure(&self, config: ListenerConfig) {
        let _ = self.commands.send(ListenerCommand::Configure(config));
    }

    fn stop(&self) {
        let _ = self.commands.send(ListenerCommand::Stop);
    }

    fn status(&self) -> Result<OscListenerStatus, String> {
        self.status.lock().map(|status| status.clone()).map_err(|error| error.to_string())
    }
}

fn tcp_accept_loop(
    mut config: ListenerConfig,
    commands: Receiver<ListenerCommand>,
    bind_ip: impl Fn() -> IpAddr,
    mut on_status: impl FnMut(OscListenerStatus),
    on_packet: PacketHandler,
    on_query: QueryHandler,
) {
    let shared_config = Arc::new(Mutex::new(config.clone()));
    let mut listener: Option<std::net::TcpListener> = None;
    let mut reported_disabled = !config.enabled;
    loop {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(ListenerCommand::Configure(new)) => {
                let rebind = new.enabled != config.enabled || new.port != config.port;
                config = new;
                if let Ok(mut shared) = shared_config.lock() {
                    *shared = config.clone();
                }
                if rebind {
                    listener = None;
                    reported_disabled = !config.enabled;
                }
            }
            Ok(ListenerCommand::Retry) => listener = None,
            Ok(ListenerCommand::Stop) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                on_status(OscListenerStatus::Disabled);
                return;
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }
        if !config.enabled {
            if listener.is_some() || !reported_disabled {
                listener = None;
                reported_disabled = true;
                on_status(OscListenerStatus::Disabled);
            }
            continue;
        }
        if listener.is_none() {
            let bind_addr = SocketAddr::new(bind_ip(), config.port);
            match std::net::TcpListener::bind(bind_addr) {
                Ok(bound) => {
                    let _ = bound.set_nonblocking(true);
                    let address = bound.local_addr().unwrap_or(bind_addr);
                    on_status(OscListenerStatus::Listening { address });
                    listener = Some(bound);
                }
                Err(error) => {
                    on_status(OscListenerStatus::BindFailed { address: bind_addr, error: error.to_string() });
                    // Retry on the next command tick; do not spin.
                    match commands.recv_timeout(Duration::from_millis(500)) {
                        Ok(ListenerCommand::Configure(new)) => {
                            config = new;
                            if let Ok(mut shared) = shared_config.lock() { *shared = config.clone(); }
                        }
                        Ok(ListenerCommand::Stop) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                        _ => {}
                    }
                    continue;
                }
            }
        }
        let Some(bound) = &listener else { continue };
        match bound.accept() {
            Ok((stream, src)) => {
                let allowed_ips = shared_config.lock().map(|c| c.allowed_ips.clone()).unwrap_or_default();
                // Same gates as UDP: allowlist, then the network input overrides.
                if !is_allowed(&allowed_ips, &src.ip().to_string())
                    || (!crate::engine::overrides::OVERRIDES.network_external_input() && !src.ip().is_loopback())
                    || (!crate::engine::overrides::OVERRIDES.network_local_input() && src.ip().is_loopback())
                {
                    log::debug!("OSC TCP: refusing connection from {src}");
                    continue;
                }
                let config = shared_config.clone();
                let on_packet = on_packet.clone();
                let on_query = on_query.clone();
                std::thread::Builder::new()
                    .name(format!("inkue-osc-tcp-{src}"))
                    .spawn(move || tcp_connection_loop(stream, src, config, on_packet, on_query))
                    .ok();
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => listener = None,
        }
    }
}

fn tcp_connection_loop(
    mut stream: std::net::TcpStream,
    src: SocketAddr,
    config: Arc<Mutex<ListenerConfig>>,
    on_packet: PacketHandler,
    on_query: QueryHandler,
) {
    // QLab never auto-disconnects TCP clients: pin this peer so the per-sender
    // registries (updates, alwaysReply, reply port/format) survive indefinitely.
    crate::engine::osc_updates::pin_client(src, true);
    let writer = stream.try_clone().ok().map(|s| Arc::new(Mutex::new(s)));
    if let Some(writer) = &writer {
        if let Ok(mut guard) = TCP_CONNECTIONS.lock() {
            guard.get_or_insert_with(HashMap::new).insert(src, writer.clone());
        }
    }
    let reply = |bytes: &[u8]| {
        if let Some(writer) = &writer {
            if let Ok(mut socket) = writer.lock() {
                use std::io::Write;
                let _ = socket.write_all(&slip_encode(bytes));
                let _ = socket.flush();
            }
        }
    };
    let mut slip = SlipDecoder::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match std::io::Read::read(&mut stream, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                for payload in slip.feed(&buf[..n]) {
                    let Ok((_, packet)) = rosc::decoder::decode_udp(&payload) else { continue };
                    let allowed_ips = config.lock().map(|c| c.allowed_ips.clone()).unwrap_or_default();
                    handle_decoded_packet(&mut Dispatch {
                        protocol: Protocol::Osc,
                        allowed_ips: &allowed_ips,
                        src,
                        dedup: None,
                        reply: &reply,
                        on_packet: &mut |p, a, s| on_packet(p, a, s),
                        on_query: &mut |q, s| on_query(q, s),
                        label: "OSC TCP",
                    }, &packet, &payload);
                }
            }
            Err(_) => break,
        }
    }
    if let Ok(mut guard) = TCP_CONNECTIONS.lock() {
        if let Some(map) = guard.as_mut() {
            map.remove(&src);
        }
    }
    apply_client_session(ClientSessionAction::Disconnect, src);
}

/// Clients that sent `/alwaysReply 1` get a `/reply{addr}` for every message.
/// Same 61 s UDP keepalive as `/updates` subscribers (any packet refreshes).
static ALWAYS_REPLY: Mutex<Option<HashMap<SocketAddr, Instant>>> = Mutex::new(None);

/// QLab `/udpReplyPort {0..65535}`: per-sender port that receives UDP replies.
/// `0` (or removal) restores the default of replying to the sender's own port.
/// Same 61 s keepalive as the other per-sender registries; pinned clients keep theirs.
static REPLY_PORTS: Mutex<Option<HashMap<SocketAddr, (u16, Instant)>>> = Mutex::new(None);

fn set_reply_port(addr: SocketAddr, port: Option<u16>) {
    if let Ok(mut guard) = REPLY_PORTS.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        match port {
            Some(port) => { map.insert(addr, (port, Instant::now())); }
            None => { map.remove(&addr); }
        }
    }
}

/// The port this sender currently receives UDP replies on (its own port by default).
fn current_reply_port(addr: SocketAddr) -> u16 {
    let Ok(mut guard) = REPLY_PORTS.lock() else { return addr.port() };
    let Some(map) = guard.as_mut() else { return addr.port() };
    let now = Instant::now();
    map.retain(|peer, (_, seen)| crate::engine::osc_updates::is_pinned(*peer)
        || now.duration_since(*seen) < Duration::from_secs(61));
    map.get(&addr).map(|(port, _)| *port).unwrap_or_else(|| addr.port())
}

/// Where to send a UDP reply for this sender, honoring `/udpReplyPort`.
pub(crate) fn reply_target(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip(), current_reply_port(addr))
}

/// QLab `/replyFormat {string}`: per-sender reply payload format with `#token#`
/// placeholders; an empty string resets to the default JSON envelope. Same 61 s
/// keepalive as the other per-sender registries; pinned clients keep theirs.
static REPLY_FORMATS: Mutex<Option<HashMap<SocketAddr, (String, Instant)>>> = Mutex::new(None);

fn set_reply_format(addr: SocketAddr, format: Option<String>) {
    if let Ok(mut guard) = REPLY_FORMATS.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        match format {
            Some(format) if !format.is_empty() => { map.insert(addr, (format, Instant::now())); }
            _ => { map.remove(&addr); }
        }
    }
}

fn current_reply_format(addr: SocketAddr) -> Option<String> {
    let Ok(mut guard) = REPLY_FORMATS.lock() else { return None };
    let map = guard.as_mut()?;
    let now = Instant::now();
    map.retain(|peer, (_, seen)| crate::engine::osc_updates::is_pinned(*peer)
        || now.duration_since(*seen) < Duration::from_secs(61));
    map.get(&addr).map(|(format, _)| format.clone())
}

fn set_always_reply(addr: SocketAddr, on: bool) {
    if let Ok(mut guard) = ALWAYS_REPLY.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        if on { map.insert(addr, Instant::now()); } else { map.remove(&addr); }
    }
}

fn always_reply_enabled(addr: SocketAddr) -> bool {
    let Ok(mut guard) = ALWAYS_REPLY.lock() else { return false };
    let Some(map) = guard.as_mut() else { return false };
    let now = Instant::now();
    map.retain(|peer, seen| crate::engine::osc_updates::is_pinned(*peer)
        || now.duration_since(*seen) < Duration::from_secs(61));
    map.contains_key(&addr)
}

fn touch_always_reply(addr: SocketAddr) {
    if let Ok(mut guard) = ALWAYS_REPLY.lock() {
        if let Some(map) = guard.as_mut() {
            if let Some(seen) = map.get_mut(&addr) { *seen = Instant::now(); }
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
            [one] if address.starts_with("/cue/") || address.starts_with("/cue_id/")
                || address == "/updates" || address.ends_with("/updates")
                || address == "/alwaysReply" || address.ends_with("/alwaysReply")
                || address == "/forgetMeNot" || address.ends_with("/forgetMeNot")
                || address == "/udpKeepAlive" || address.ends_with("/udpKeepAlive")
                || address == "/udpReplyPort" || address.ends_with("/udpReplyPort")
                || address == "/overrideWindow" || address.ends_with("/overrideWindow")
                || address == "/replyFormat" || address.ends_with("/replyFormat")
                || address == "/eventFormat" || address.ends_with("/eventFormat")
                || address == "/workingDirectory" => {
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

fn handle_packet(packet: &rosc::OscPacket, explicitly_allowed: bool, src: SocketAddr, app_handle: &tauri::AppHandle) {
    match packet {
        rosc::OscPacket::Message(msg) => handle_message(msg, explicitly_allowed, src, app_handle),
        rosc::OscPacket::Bundle(bundle) => {
            for p in &bundle.content {
                handle_packet(p, explicitly_allowed, src, app_handle);
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
    /// QLab `/updates {0|1}`: subscribe/unsubscribe the sender to push updates.
    Updates(bool),
    /// QLab `/alwaysReply {0|1}`: reply to every message from this sender.
    AlwaysReply(bool),
    /// QLab `/forgetMeNot` | `/udpKeepAlive {0|1}` and `/disconnect`: client session.
    ClientSession { action: ClientSessionAction },
    /// QLab `/udpReplyPort {0..65535}`: set this sender's UDP reply port.
    SetReplyPort(u16),
    /// QLab `/overrideWindow {bool}` and `/toggleOverrideWindow` (`None` toggles).
    SetOverrideWindow(Option<bool>),
    /// QLab `/replyFormat {string}`: set this sender's reply payload format.
    SetReplyFormat(String),
    /// QLab show-control broadcast: `/listen[/{event}[/{detail}]]` / `/ignore/...`.
    Broadcast { listen: bool, event: Option<String> },
    /// QLab `/eventFormat {string}`: custom show-control broadcast format.
    SetEventFormat(String),
    /// QLab `/workingDirectory {path}`: set the application working directory.
    SetWorkingDirectory(String),
    /// Trigger a full cue-list feedback dump.
    CueListRequest,
    /// Trigger a playhead feedback dump.
    PlayheadRequest,
    /// Set or toggle a global engine override (QLab `/overrides/...`).
    SetOverride { key: &'static str, value: Option<bool> },
    /// Address (or its arguments) does not map to any Inkue command.
    Unmatched,
}

#[derive(Clone, Copy, PartialEq)]
enum ClientSessionAction {
    /// QLab `/forgetMeNot {bool}` / `/udpKeepAlive {bool}`: pin beyond the 61 s keepalive.
    Pin(bool),
    /// QLab `/disconnect`: drop the sender from every client registry.
    Disconnect,
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

fn apply_reply_port(port: u16, src: SocketAddr) {
    set_reply_port(src, if port == 0 { None } else { Some(port) });
}

fn apply_client_session(action: ClientSessionAction, src: SocketAddr) {
    match action {
        ClientSessionAction::Pin(on) => crate::engine::osc_updates::pin_client(src, on),
        ClientSessionAction::Disconnect => {
            crate::engine::osc_updates::disconnect_client(src);
            crate::engine::osc_broadcast::disconnect_client(src);
            set_always_reply(src, false);
            set_reply_port(src, None);
            set_reply_format(src, None);
        }
    }
}

fn handle_message(msg: &rosc::OscMessage, explicitly_allowed: bool, src: SocketAddr, app_handle: &tauri::AppHandle) {
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
        OscAction::Updates(enabled) => {
            crate::engine::osc_updates::set_subscription(src, enabled);
            (true, false)
        }
        OscAction::AlwaysReply(_) => (true, false),
        OscAction::ClientSession { action } => {
            apply_client_session(action, src);
            (true, false)
        }
        OscAction::SetReplyPort(port) => {
            apply_reply_port(port, src);
            (true, false)
        }
        OscAction::SetOverrideWindow(open) => {
            let open = open.unwrap_or_else(|| !crate::engine::overrides::OVERRIDES.override_window());
            crate::engine::overrides::OVERRIDES.set_override_window(open);
            let _ = app_handle.emit("override-window-changed", open);
            (true, false)
        }
        OscAction::SetReplyFormat(format) => {
            set_reply_format(src, Some(format));
            (true, false)
        }
        OscAction::Broadcast { listen, event } => {
            if listen {
                crate::engine::osc_broadcast::listen(src, event.as_deref());
            } else {
                crate::engine::osc_broadcast::ignore(src, event.as_deref());
            }
            (true, false)
        }
        OscAction::SetEventFormat(format) => {
            crate::engine::osc_broadcast::set_event_format(src, Some(format));
            (true, false)
        }
        OscAction::SetWorkingDirectory(path) => {
            match set_working_directory(&path) {
                Ok(_) => (true, false),
                Err(error) => {
                    log::warn!("OSC /workingDirectory rejected: {error}");
                    (false, false)
                }
            }
        }
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
        | OscAction::SetOverride { .. }
        | OscAction::SetWorkingDirectory(_) if !explicitly_allowed => OscAction::Unmatched,
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

/// QLab cue verbs that write a saved cue field: `/cue/{n}/preWait 5`,
/// `/cue/{n}/armed 0`, ... take exactly one argument, except the boolean
/// `toggle` forms (`/cue/{n}/flagged/toggle`) which take none.
fn cue_write_action(verb: &str, args: &[rosc::OscType]) -> Option<NetworkCueAction> {
    // QLab `/live` form: "live" is always the last address segment
    // (`/cue/5/opacity/+/live 10` works, `/live/+` does not).
    let (verb, live) = match verb.strip_suffix("/live") {
        Some(core) => (core, true),
        None => (verb, false),
    };
    // Media-cue properties: `/cue/{n}/rate {number}` and
    // `/cue/{n}/level/{row}/{col} {db}` (each also in `/live` form).
    if verb == "rate" {
        return match args {
            [one] => numeric_arg(std::slice::from_ref(one))
                .filter(|value| value.is_finite() && *value > 0.0)
                .map(|value| NetworkCueAction::SetRate { value, live }),
            _ => None,
        };
    }
    if let Some(rest) = verb.strip_prefix("level/") {
        let mut parts = rest.split('/');
        let (Some(row), Some(col), None) = (parts.next(), parts.next(), parts.next()) else {
            return None;
        };
        let (Ok(row), Ok(col)) = (row.parse::<u32>(), col.parse::<u32>()) else { return None };
        if row >= 128 || col >= 128 {
            return None;
        }
        return match args {
            [one] => numeric_arg(std::slice::from_ref(one))
                .filter(|db| db.is_finite())
                .map(|db| NetworkCueAction::SetLevel { row, col, db, live }),
            _ => None,
        };
    }
    // QLab video-cue properties: opacity 0..=1, layer 1..=1000, blendMode name,
    // saturation 0..=2, scale > 0, rotation 0..360, translateX/Y -1..=1.
    let video_prop = match verb {
        "opacity" => Some(VideoProp::Opacity),
        "layer" => Some(VideoProp::Layer),
        "blendMode" => Some(VideoProp::BlendMode),
        "saturation" => Some(VideoProp::Saturation),
        "scale" => Some(VideoProp::Scale),
        "rotation" => Some(VideoProp::Rotation),
        "translateX" => Some(VideoProp::PanX),
        "translateY" => Some(VideoProp::PanY),
        _ => None,
    };
    if let Some(prop) = video_prop {
        let value = match (prop, args) {
            (VideoProp::BlendMode, [rosc::OscType::String(name)]) => {
                crate::engine::output_engine::BlendMode::from_osc_name(name)?;
                VideoPropValue::Text(name.clone())
            }
            (_, [one]) => {
                let v = numeric_arg(std::slice::from_ref(one))?;
                let in_range = match prop {
                    VideoProp::Opacity => (0.0..=1.0).contains(&v),
                    VideoProp::Saturation => (0.0..=2.0).contains(&v),
                    VideoProp::Layer => (1.0..=1000.0).contains(&v) && v.fract() == 0.0,
                    VideoProp::Scale => v.is_finite() && v > 0.0,
                    VideoProp::Rotation => (0.0..=360.0).contains(&v),
                    VideoProp::PanX | VideoProp::PanY => (-1.0..=1.0).contains(&v),
                    VideoProp::BlendMode => false,
                };
                if !v.is_finite() || !in_range {
                    return None;
                }
                VideoPropValue::Num(v)
            }
            _ => return None,
        };
        return Some(NetworkCueAction::SetVideoProp { prop, value, live });
    }
    let (verb, toggle_suffix) = match verb.split_once('/') {
        Some((core, "toggle")) => (core, true),
        _ => (verb, false),
    };
    if verb == "flagged" {
        return match args {
            [] if toggle_suffix => Some(NetworkCueAction::SetFlagged(None)),
            [] => None,
            [one] if !toggle_suffix => match one {
                rosc::OscType::String(text) if text.eq_ignore_ascii_case("toggle") =>
                    Some(NetworkCueAction::SetFlagged(None)),
                other => qlab_bool(other).map(|flag| NetworkCueAction::SetFlagged(Some(flag))),
            },
            _ => None,
        };
    }
    // QLab 5.5+ increment/decrement: `/cue/{n}/preWait/+ {delta}` and the
    // embedded form `/cue/{n}/preWait/+/1` (delta carried in the address).
    if let Some((core, sign, embedded)) = increment_verb(verb) {
        let delta = match (embedded, args) {
            (Some(text), []) => text.parse::<f64>().ok()?,
            (None, [one]) => numeric_arg(std::slice::from_ref(one))?,
            _ => return None,
        };
        if !delta.is_finite() {
            return None;
        }
        return match core {
            "preWait" => Some(NetworkCueAction::AdjustPreWait(sign * delta)),
            "postWait" => Some(NetworkCueAction::AdjustPostWait(sign * delta)),
            "continueMode" if delta.fract() == 0.0 =>
                Some(NetworkCueAction::AdjustContinueMode((sign * delta) as i64)),
            _ => None,
        };
    }
    if toggle_suffix || args.len() != 1 {
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
        "notes" => match &args[0] {
            rosc::OscType::String(notes) if notes.len() <= MAX_NOTES_WRITE_BYTES =>
                Some(NetworkCueAction::SetNotes(notes.clone())),
            _ => None,
        },
        "colorName" => match &args[0] {
            rosc::OscType::String(name) =>
                crate::cue::types::CueColor::from_osc_name(name).map(NetworkCueAction::SetColor),
            _ => None,
        },
        _ => None,
    }
}

/// Split a QLab 5.5 increment/decrement verb (`preWait/+`, `preWait/+/1`) into
/// the property core, the sign, and an optional address-embedded delta.
fn increment_verb(verb: &str) -> Option<(&str, f64, Option<&str>)> {
    let (core, rest) = verb.split_once('/')?;
    match rest.split_once('/') {
        Some(("+", delta)) => Some((core, 1.0, Some(delta))),
        Some(("-", delta)) => Some((core, -1.0, Some(delta))),
        None if rest == "+" => Some((core, 1.0, None)),
        None if rest == "-" => Some((core, -1.0, None)),
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
        .is_some_and(|(_, verb)| {
            let core = verb.split('/').next().unwrap_or(verb);
            matches!(core, "preWait" | "postWait" | "armed" | "qName" | "continueMode"
                | "notes" | "colorName" | "flagged" | "load" | "panic" | "rate" | "level"
                | "opacity" | "layer" | "blendMode" | "saturation" | "scale" | "rotation"
                | "translateX" | "translateY")
        });
    let updates_arg = addr == "/updates" || addr.ends_with("/updates")
        || addr == "/alwaysReply" || addr.ends_with("/alwaysReply")
        || addr == "/forgetMeNot" || addr.ends_with("/forgetMeNot")
        || addr == "/udpKeepAlive" || addr.ends_with("/udpKeepAlive")
        || addr == "/udpReplyPort" || addr.ends_with("/udpReplyPort")
        || addr == "/overrideWindow" || addr.ends_with("/overrideWindow")
        || addr == "/replyFormat" || addr.ends_with("/replyFormat")
        || addr == "/eventFormat" || addr.ends_with("/eventFormat")
        || addr == "/workingDirectory";
    if !args.is_empty() && !cue_write && !updates_arg {
        return OscAction::Unmatched;
    }
    let Some((workspace, addr)) = WorkspaceSelector::split_address(addr) else {
        return OscAction::Unmatched;
    };
    use NetworkWorkspaceAction as Transport;
    if addr == "/thump" {
        return OscAction::Heartbeat;
    }
    // QLab `/alwaysReply {bool}` and `/updates {bool}` work at any access level.
    if addr == "/alwaysReply" || addr == "/updates" {
        let _ = workspace;
        return match args {
            [one] => match qlab_bool(one) {
                Some(on) if addr == "/updates" => OscAction::Updates(on),
                Some(on) => OscAction::AlwaysReply(on),
                None => OscAction::Unmatched,
            },
            // The no-argument form is the read, parsed as a query instead.
            _ => OscAction::Unmatched,
        };
    }
    // QLab `/disconnect` and `/forgetMeNot`/`/udpKeepAlive {bool}` manage the
    // sending client at any access level; no-arg forgetMeNot is the read form.
    if addr == "/disconnect" {
        let _ = workspace;
        return match args {
            [] => OscAction::ClientSession { action: ClientSessionAction::Disconnect },
            _ => OscAction::Unmatched,
        };
    }
    if addr == "/forgetMeNot" || addr == "/udpKeepAlive" {
        let _ = workspace;
        return match args {
            [one] => match qlab_bool(one) {
                Some(on) => OscAction::ClientSession { action: ClientSessionAction::Pin(on) },
                None => OscAction::Unmatched,
            },
            _ => OscAction::Unmatched,
        };
    }
    // QLab show-control broadcast subscriptions: `/listen`, `/ignore`, and the
    // scoped `/{command}/{event}[/{detail}]` forms (any access level).
    if let Some(rest) = addr.strip_prefix("/listen").or_else(|| addr.strip_prefix("/ignore")) {
        let listen = addr.starts_with("/listen");
        let path = rest.strip_prefix('/').unwrap_or(rest);
        if path.is_empty() {
            let _ = workspace;
            return match args {
                [] => OscAction::Broadcast { listen, event: None },
                _ => OscAction::Unmatched,
            };
        }
        if !args.is_empty() {
            return OscAction::Unmatched;
        }
        // Match the longest known event first so `/listen/cue/start/number`
        // resolves to the `cue/start` event with the `number` detail.
        let mut events: Vec<&str> = crate::engine::osc_broadcast::CUE_EVENTS
            .iter().chain(crate::engine::osc_broadcast::BARE_EVENTS.iter()).copied().collect();
        events.sort_by_key(|event| std::cmp::Reverse(event.len()));
        for event in events {
            if path == event {
                let _ = workspace;
                return OscAction::Broadcast { listen, event: Some(event.to_string()) };
            }
            if let Some(detail) = path.strip_prefix(event).and_then(|rest| rest.strip_prefix('/')) {
                if crate::engine::osc_broadcast::is_known_detail(detail) {
                    let _ = workspace;
                    return OscAction::Broadcast { listen, event: Some(event.to_string()) };
                }
            }
        }
        let _ = workspace;
        return OscAction::Unmatched;
    }
    // QLab `/eventFormat {string}`; the no-argument form is the read (a query).
    if addr == "/eventFormat" {
        let _ = workspace;
        return match args {
            [rosc::OscType::String(format)] => OscAction::SetEventFormat(format.clone()),
            _ => OscAction::Unmatched,
        };
    }
    // QLab `/workingDirectory {path}` (application-level, unscoped only); the
    // no-argument form is the read (a query).
    if addr == "/workingDirectory" {
        let _ = workspace;
        return match args {
            [rosc::OscType::String(path)] => OscAction::SetWorkingDirectory(path.clone()),
            _ => OscAction::Unmatched,
        };
    }
    // QLab `/replyFormat {string}`; the no-argument form is the read (a query).
    if addr == "/replyFormat" {
        let _ = workspace;
        return match args {
            [rosc::OscType::String(format)] => OscAction::SetReplyFormat(format.clone()),
            _ => OscAction::Unmatched,
        };
    }
    // QLab `/overrideWindow {bool}` (no-arg is the read, a query) and
    // `/toggleOverrideWindow`. Any access level, like the other app controls.
    if addr == "/overrideWindow" || addr == "/toggleOverrideWindow" {
        let _ = workspace;
        let toggle = addr == "/toggleOverrideWindow";
        return match args {
            [] if toggle => OscAction::SetOverrideWindow(None),
            [one] if !toggle => match qlab_bool(one) {
                Some(on) => OscAction::SetOverrideWindow(Some(on)),
                None => OscAction::Unmatched,
            },
            _ => OscAction::Unmatched,
        };
    }
    // QLab `/udpReplyPort {0..65535}`; the no-argument form is the read (a query).
    if addr == "/udpReplyPort" {
        let _ = workspace;
        return match args {
            [one] => match numeric_arg(std::slice::from_ref(one)) {
                Some(value) if value.fract() == 0.0 && (0.0..=65535.0).contains(&value) =>
                    OscAction::SetReplyPort(value as u16),
                _ => OscAction::Unmatched,
            },
            _ => OscAction::Unmatched,
        };
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
        "/stop" => Some(Transport::Stop),
        "/panic" => Some(Transport::Panic),
        "/hardStop" => Some(Transport::HardStop),
        "/pause" => Some(Transport::SetPaused(true)),
        "/resume" => Some(Transport::SetPaused(false)),
        "/togglePause" => Some(Transport::TogglePaused),
        "/reset" => Some(Transport::Reset),
        "/save" => Some(Transport::Save),
        "/undo" => Some(Transport::Undo),
        "/redo" => Some(Transport::Redo),
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
            "hardStop" if args.is_empty() => (id, NetworkCueAction::HardStop),
            "devamp" if args.is_empty() => (id, NetworkCueAction::Devamp),
            "togglePause" if args.is_empty() => (id, NetworkCueAction::SetPause(NetworkPauseAction::Toggle)),
            // QLab `/cue_id/{id}/panic [seconds]`: fade out and stop; a second
            // panic while fading hard-stops immediately.
            "panic" => match args {
                [] => (id, NetworkCueAction::Panic(None)),
                [one] => match numeric_arg(std::slice::from_ref(one)) {
                    Some(secs) if secs.is_finite() && secs >= 0.0 => (id, NetworkCueAction::Panic(Some(secs))),
                    _ => return OscAction::Unmatched,
                },
                _ => return OscAction::Unmatched,
            },
            "load" => match args {
                [] => (id, NetworkCueAction::Load(0.0)),
                [one] => match numeric_arg(std::slice::from_ref(one)) {
                    Some(secs) if secs.is_finite() => (id, NetworkCueAction::Load(secs)),
                    _ => return OscAction::Unmatched,
                },
                _ => return OscAction::Unmatched,
            },
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
                "hardStop" if args.is_empty() => NetworkCueAction::HardStop,
                "togglePause" if args.is_empty() => NetworkCueAction::SetPause(NetworkPauseAction::Toggle),
                "panic" if args.is_empty() => NetworkCueAction::Panic(None),
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
            "hardStop" if args.is_empty() => (number, NetworkCueAction::HardStop),
            "devamp" if args.is_empty() => (number, NetworkCueAction::Devamp),
            "togglePause" if args.is_empty() => (number, NetworkCueAction::SetPause(NetworkPauseAction::Toggle)),
            // QLab `/cue/{n}/panic [seconds]`: fade out and stop the cue.
            "panic" => match args {
                [] => (number, NetworkCueAction::Panic(None)),
                [one] => match numeric_arg(std::slice::from_ref(one)) {
                    Some(secs) if secs.is_finite() && secs >= 0.0 => (number, NetworkCueAction::Panic(Some(secs))),
                    _ => return OscAction::Unmatched,
                },
                _ => return OscAction::Unmatched,
            },
            "load" => match args {
                [] => (number, NetworkCueAction::Load(0.0)),
                [one] => match numeric_arg(std::slice::from_ref(one)) {
                    Some(secs) if secs.is_finite() => (number, NetworkCueAction::Load(secs)),
                    _ => return OscAction::Unmatched,
                },
                _ => return OscAction::Unmatched,
            },
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
/// Map an override address core to its registry key. The `toggle` prefix leaves
/// the core capitalized ("DmxOutput"); the `Enabled` suffix form keeps QLab's
/// camelCase ("dmxOutput").
fn override_key(core: &str) -> Option<&'static str> {
    match core {
        "dmxOutput" | "DmxOutput" => Some("dmx_output"),
        "midiInput" | "MidiInput" => Some("midi_input"),
        "midiOutput" | "MidiOutput" => Some("midi_output"),
        "mscInput" | "MscInput" => Some("msc_input"),
        "mscOutput" | "MscOutput" => Some("msc_output"),
        "networkExternalInput" | "NetworkExternalInput" => Some("network_external_input"),
        "networkExternalOutput" | "NetworkExternalOutput" => Some("network_external_output"),
        "networkLocalInput" | "NetworkLocalInput" => Some("network_local_input"),
        "networkLocalOutput" | "NetworkLocalOutput" => Some("network_local_output"),
        "sysexInput" | "SysexInput" => Some("sysex_input"),
        "sysexOutput" | "SysexOutput" => Some("sysex_output"),
        "timecodeInput" | "TimecodeInput" => Some("timecode_input"),
        "timecodeOutput" | "TimecodeOutput" => Some("timecode_output"),
        _ => None,
    }
}

/// The `/overrides/{x}Enabled` form without `/toggle`: QLab's read form, which
/// `CueQuery::parse` turns into a query rather than a command.
fn override_enabled_key(name: &str) -> Option<&'static str> {
    override_key(name.strip_suffix("Enabled")?)
}

fn parse_override_address(rest: &str, args: &[rosc::OscType]) -> OscAction<'static> {
    let (name, suffix) = match rest.split_once('/') {
        Some((name, suffix)) => (name, Some(suffix)),
        None => (rest, None),
    };
    if suffix.is_some() && suffix != Some("toggle") {
        return OscAction::Unmatched;
    }
    let (core, mut toggle_name) = if let Some(core) = name.strip_prefix("toggle") {
        (core, true)
    } else if let Some(core) = name.strip_suffix("Enabled") {
        (core, false)
    } else {
        return OscAction::Unmatched;
    };
    toggle_name = toggle_name || suffix == Some("toggle");
    let Some(key) = override_key(core) else {
        return OscAction::Unmatched;
    };
    let value = match args {
        // No argument on the `Enabled` form is the read, handled as a query.
        [] if !toggle_name => return OscAction::Unmatched,
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
/// QLab `/cue/{n}/notes` read/write cap, so a huge note cannot blow up a UDP reply.
const MAX_NOTES_QUERY_BYTES: usize = 8_192;
const MAX_NOTES_WRITE_BYTES: usize = 8_192;
/// Recursion cap for cue-dictionary `cues` children.
const MAX_CUE_DICT_DEPTH: usize = 16;
/// Shared empty flagged set for cues whose owning list cannot be identified.
static EMPTY_FLAGGED: std::collections::BTreeSet<CueId> = std::collections::BTreeSet::new();

/// Build QLab's cue dictionary for `/cueLists` entries and `/children` replies.
/// `rows` counts every emitted cue so huge shows error instead of flooding UDP.
fn cue_dict(
    cue: &dyn Cue,
    list_name: &str,
    flagged: &std::collections::BTreeSet<CueId>,
    rows: &mut usize,
    depth: usize,
) -> serde_json::Value {
    *rows += 1;
    let children = if depth < MAX_CUE_DICT_DEPTH {
        cue.child_cues().map(|children| children.iter()
            .map(|child| cue_dict(child.as_ref(), list_name, flagged, rows, depth + 1))
            .collect::<Vec<_>>())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    serde_json::json!({
        "uniqueID": cue.id().to_string(),
        "number": cue.number().unwrap_or(""),
        "name": cue.name(),
        "listName": list_name,
        "type": cue.cue_type().qlab_name(),
        "colorName": cue.color().osc_name(),
        "flagged": flagged.contains(&cue.id()),
        "armed": !cue.is_disabled(),
        "cues": children,
    })
}

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
    /// QLab `/cue/{n}/notes` (string).
    Notes,
    /// QLab `/cue/{n}/colorName` (color name string).
    ColorName,
    /// QLab `/cue/{n}/flagged` (bool; a cue-list property).
    Flagged,
    /// QLab `/cue/{n}/children`: array of QLab cue dictionaries for group children.
    Children,
    /// QLab `/cue/{n}/rate[/live]`: playback rate (media cues).
    Rate { live: bool },
    /// QLab `/cue/{n}/level/{row}/{col}[/live]`: crosspoint level in dB.
    Level { row: u32, col: u32, live: bool },
    /// QLab video-cue property read (`/cue/{n}/opacity[/live]`, ...).
    Video { prop: VideoProp, live: bool },
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
    /// QLab app-level queries (never workspace-scoped, always accepted).
    App(AppQueryKind),
    /// QLab per-sender status reads: `/alwaysReply` and `/updates` without an argument.
    Sender(SenderQuery),
}

#[derive(Clone, Copy, PartialEq)]
enum AppQueryKind {
    /// QLab `/version`: the running app version string.
    Version,
    /// QLab `/workspaces`: one entry per open workspace; Inkue runs one per instance.
    Workspaces,
    /// QLab `/overrides/{x}Enabled` read form: the current override state.
    Override(&'static str),
    /// QLab `/overrideWindow` read form: is the Override window visible?
    OverrideWindow,
    /// QLab `/fontNames`: array of installed font names.
    FontNames,
    /// QLab `/fontFamiliesAndStyles`: dictionary of family → styles array.
    FontFamiliesAndStyles,
    /// QLab `/workingDirectory` read form: the current working directory.
    WorkingDirectory,
}

#[derive(Clone, Copy, PartialEq)]
enum SenderQuery {
    /// QLab `/alwaysReply` read form: this sender's alwaysReply status.
    AlwaysReply,
    /// QLab `/updates` read form: this sender's subscription status.
    Updates,
    /// QLab `/forgetMeNot`/`/udpKeepAlive` read form: is this sender pinned?
    Pinned,
    /// QLab `/udpReplyPort` read form: this sender's current reply port.
    ReplyPort,
    /// QLab `/replyFormat` read form: this sender's current reply format ("" = default).
    ReplyFormat,
    /// QLab `/eventFormat` read form: this sender's broadcast format ("" = default).
    EventFormat,
}

struct CueQuery<'a> {
    address: &'a str,
    workspace: Option<WorkspaceSelector<'a>>,
    kind: CueQueryKind<'a>,
}

/// QLab `/workingDirectory`: the explicitly set directory, if any. The read
/// form falls back to the open workspace's folder, then the user's home.
static WORKING_DIRECTORY: Mutex<Option<PathBuf>> = Mutex::new(None);

fn home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn working_directory(workspace: &Workspace) -> PathBuf {
    if let Ok(guard) = WORKING_DIRECTORY.lock() {
        if let Some(path) = guard.as_ref() {
            return path.clone();
        }
    }
    workspace
        .file_path
        .as_deref()
        .and_then(|path| path.parent().map(PathBuf::from))
        .unwrap_or_else(home_directory)
}

/// QLab `/workingDirectory {path}` write: tilde expands to the home directory;
/// the target must be an existing directory. Control-level (allowlist) only.
fn set_working_directory(path: &str) -> Result<PathBuf, String> {
    let expanded = if path == "~" {
        home_directory()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home_directory().join(rest)
    } else {
        PathBuf::from(path)
    };
    let canonical = expanded
        .canonicalize()
        .map_err(|error| format!("working directory {path:?}: {error}"))?;
    if !canonical.is_dir() {
        return Err(format!("working directory {path:?} is not a directory"));
    }
    if let Ok(mut guard) = WORKING_DIRECTORY.lock() {
        *guard = Some(canonical.clone());
    }
    Ok(canonical)
}

impl<'a> CueQuery<'a> {
    fn parse(message: &'a rosc::OscMessage) -> Option<Self> {
        if !message.args.is_empty() {
            return None;
        }
        let (workspace, path) = WorkspaceSelector::split_address(&message.addr)?;
        let path = path.strip_prefix('/')?;
        // App-level queries `/version`, `/workspaces` and `/overrides/{x}Enabled`
        // exist only unscoped; QLab always accepts them, even without a passcode.
        if workspace.is_none() {
            if let Some(app) = match path {
                "version" => Some(AppQueryKind::Version),
                "workspaces" => Some(AppQueryKind::Workspaces),
                "overrideWindow" => Some(AppQueryKind::OverrideWindow),
                "fontNames" => Some(AppQueryKind::FontNames),
                "fontFamiliesAndStyles" => Some(AppQueryKind::FontFamiliesAndStyles),
                "workingDirectory" => Some(AppQueryKind::WorkingDirectory),
                _ => path.strip_prefix("overrides/").and_then(override_enabled_key).map(AppQueryKind::Override),
            } {
                return Some(Self { address: &message.addr, workspace, kind: CueQueryKind::App(app) });
            }
        }
        // `/alwaysReply`, `/updates` and `/forgetMeNot`/`/udpKeepAlive` without an
        // argument read the sender's status.
        if let Some(sender) = match path {
            "alwaysReply" => Some(SenderQuery::AlwaysReply),
            "updates" => Some(SenderQuery::Updates),
            "forgetMeNot" | "udpKeepAlive" => Some(SenderQuery::Pinned),
            "udpReplyPort" => Some(SenderQuery::ReplyPort),
            "replyFormat" => Some(SenderQuery::ReplyFormat),
            "eventFormat" => Some(SenderQuery::EventFormat),
            _ => None,
        } {
            return Some(Self { address: &message.addr, workspace, kind: CueQueryKind::Sender(sender) });
        }
        // Workspace-level queries next: `/cueLists`, `/cueList/{id}/playbackPosition`.
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
            "notes" => CueQueryProperty::Notes,
            "colorName" => CueQueryProperty::ColorName,
            "flagged" => CueQueryProperty::Flagged,
            "children" => CueQueryProperty::Children,
            "rate" => CueQueryProperty::Rate { live: false },
            "rate/live" => CueQueryProperty::Rate { live: true },
            "opacity" => CueQueryProperty::Video { prop: VideoProp::Opacity, live: false },
            "opacity/live" => CueQueryProperty::Video { prop: VideoProp::Opacity, live: true },
            "layer" => CueQueryProperty::Video { prop: VideoProp::Layer, live: false },
            "layer/live" => CueQueryProperty::Video { prop: VideoProp::Layer, live: true },
            "blendMode" => CueQueryProperty::Video { prop: VideoProp::BlendMode, live: false },
            "blendMode/live" => CueQueryProperty::Video { prop: VideoProp::BlendMode, live: true },
            "saturation" => CueQueryProperty::Video { prop: VideoProp::Saturation, live: false },
            "saturation/live" => CueQueryProperty::Video { prop: VideoProp::Saturation, live: true },
            "scale" => CueQueryProperty::Video { prop: VideoProp::Scale, live: false },
            "scale/live" => CueQueryProperty::Video { prop: VideoProp::Scale, live: true },
            "rotation" => CueQueryProperty::Video { prop: VideoProp::Rotation, live: false },
            "rotation/live" => CueQueryProperty::Video { prop: VideoProp::Rotation, live: true },
            "translateX" => CueQueryProperty::Video { prop: VideoProp::PanX, live: false },
            "translateX/live" => CueQueryProperty::Video { prop: VideoProp::PanX, live: true },
            "translateY" => CueQueryProperty::Video { prop: VideoProp::PanY, live: false },
            "translateY/live" => CueQueryProperty::Video { prop: VideoProp::PanY, live: true },
            property if property.starts_with("level/") => {
                let (rest, live) = match property.strip_suffix("/live") {
                    Some(core) => (core, true),
                    None => (property, false),
                };
                let mut parts = rest.strip_prefix("level/")?.split('/');
                let (Some(row), Some(col), None) = (parts.next(), parts.next(), parts.next()) else {
                    return None;
                };
                let (Ok(row), Ok(col)) = (row.parse::<u32>(), col.parse::<u32>()) else { return None };
                if row >= 128 || col >= 128 {
                    return None;
                }
                CueQueryProperty::Level { row, col, live }
            }
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

    /// App-level and per-sender status queries carry no show data, so QLab
    /// answers them at any access level (even with no passcode configured).
    fn is_any_access(&self) -> bool {
        matches!(self.kind, CueQueryKind::App(_) | CueQueryKind::Sender(_))
    }

    fn is_single_datagram(&self, data: &[u8]) -> bool {
        // The decoder accepts some malformed empty type tags. Require the exact
        // address padding and zero-argument OSC type tag before returning data.
        let end = (self.address.len() + 1).next_multiple_of(4);
        data.get(self.address.len()..end)
            .is_some_and(|padding| padding.iter().all(|byte| *byte == 0))
            && data.get(end..) == Some(b",\0\0\0")
    }

    fn read(&self, workspace: &Workspace, src: SocketAddr, port: u16) -> Option<serde_json::Value> {
        if !self.is_any_access() && workspace.cue_lists.len() > MAX_SCOPED_ROWS {
            return None;
        }
        match &self.kind {
            CueQueryKind::App(AppQueryKind::Version) =>
                Some(serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string())),
            CueQueryKind::App(AppQueryKind::Workspaces) => {
                // QLab shape: one dictionary per open workspace. Inkue replies
                // to the requesting socket, so udpReplyPort is the sender's port.
                let name = if !workspace.metadata.name.is_empty() {
                    workspace.metadata.name.clone()
                } else {
                    workspace.file_path.as_deref()
                        .and_then(|path| path.file_name())
                        .and_then(|name| name.to_str())
                        .unwrap_or("Untitled")
                        .to_string()
                };
                Some(serde_json::json!([{
                    "uniqueID": workspace.instance_id().to_string(),
                    "displayName": name,
                    "port": port,
                    "udpReplyPort": src.port(),
                    "version": env!("CARGO_PKG_VERSION"),
                }]))
            }
            CueQueryKind::App(AppQueryKind::Override(key)) => Some(serde_json::json!(
                crate::engine::overrides::OVERRIDES.get(key).unwrap_or(true)
            )),
            CueQueryKind::App(AppQueryKind::OverrideWindow) =>
                Some(serde_json::json!(crate::engine::overrides::OVERRIDES.override_window())),
            CueQueryKind::App(AppQueryKind::FontNames) =>
                Some(serde_json::json!(crate::commands::preferences_cmds::list_system_fonts())),
            CueQueryKind::App(AppQueryKind::FontFamiliesAndStyles) => {
                let families = crate::commands::preferences_cmds::font_families_and_styles();
                let map: serde_json::Map<String, serde_json::Value> = families
                    .into_iter()
                    .map(|(family, styles)| (family, serde_json::json!(styles)))
                    .collect();
                Some(serde_json::Value::Object(map))
            }
            CueQueryKind::App(AppQueryKind::WorkingDirectory) =>
                Some(serde_json::Value::String(working_directory(workspace).to_string_lossy().into_owned())),
            CueQueryKind::Sender(SenderQuery::AlwaysReply) =>
                Some(serde_json::json!(always_reply_enabled(src))),
            CueQueryKind::Sender(SenderQuery::Updates) =>
                Some(serde_json::json!(crate::engine::osc_updates::is_subscribed(src))),
            CueQueryKind::Sender(SenderQuery::Pinned) =>
                Some(serde_json::json!(crate::engine::osc_updates::is_pinned(src))),
            CueQueryKind::Sender(SenderQuery::ReplyPort) =>
                Some(serde_json::json!(current_reply_port(src))),
            CueQueryKind::Sender(SenderQuery::ReplyFormat) =>
                Some(serde_json::json!(current_reply_format(src).unwrap_or_default())),
            CueQueryKind::Sender(SenderQuery::EventFormat) =>
                Some(serde_json::json!(crate::engine::osc_broadcast::event_format(src).unwrap_or_default())),
            CueQueryKind::CueLists => {
                // QLab shape: an array of cue-list dictionaries, each with a
                // `cues` array of QLab cue dictionaries (Inkue's `id` key kept).
                let mut rows = 0usize;
                let lists = workspace.cue_lists.iter().map(|list| {
                    let cues = list.cues.iter()
                        .map(|cue| cue_dict(cue.as_ref(), &list.name, &list.flagged_cues, &mut rows, 0))
                        .collect::<Vec<_>>();
                    serde_json::json!({
                        "id": list.id.to_string(),
                        "uniqueID": list.id.to_string(),
                        "number": "",
                        "name": list.name,
                        "listName": list.name,
                        "type": "Cue List",
                        "colorName": "none",
                        "flagged": false,
                        "armed": true,
                        "cues": cues,
                    })
                }).collect::<Vec<_>>();
                (rows <= MAX_SCOPED_ROWS).then_some(serde_json::Value::Array(lists))
            }
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
            CueQueryProperty::Notes => (cue.notes().len() <= MAX_NOTES_QUERY_BYTES)
                .then(|| serde_json::Value::String(cue.notes().to_owned())),
            CueQueryProperty::ColorName =>
                Some(serde_json::Value::String(cue.color().osc_name().to_owned())),
            CueQueryProperty::Flagged => Some(serde_json::Value::Bool(
                workspace.cue_lists.iter().any(|list| list.flagged_cues.contains(&cue.id())),
            )),
            CueQueryProperty::Children => {
                let flagged = workspace.cue_lists.iter()
                    .find(|list| list.get_recursive(&cue.id()).is_some())
                    .map(|list| &list.flagged_cues);
                let list_name = workspace.active_cue_list()
                    .map(|list| list.name.clone())
                    .unwrap_or_default();
                let Some(children) = cue.child_cues() else {
                    return Some(serde_json::Value::Array(Vec::new()));
                };
                let mut rows = 0usize;
                let dicts = children.iter()
                    .map(|child| cue_dict(child.as_ref(), &list_name,
                        flagged.unwrap_or(&EMPTY_FLAGGED), &mut rows, 1))
                    .collect::<Vec<_>>();
                (rows <= MAX_SCOPED_ROWS).then_some(serde_json::Value::Array(dicts))
            }
            CueQueryProperty::Rate { live } => {
                let value = if live { cue.live_rate().or_else(|| cue.rate()) } else { cue.rate() };
                Some(value.map(|v| serde_json::json!(v)).unwrap_or(serde_json::Value::Null))
            }
            CueQueryProperty::Level { row, col, live } => {
                let (row, col) = (row as usize, col as usize);
                let value = if live { cue.live_level(row, col).or_else(|| cue.level(row, col)) }
                    else { cue.level(row, col) };
                Some(value.map(|v| serde_json::json!(v)).unwrap_or(serde_json::Value::Null))
            }
            CueQueryProperty::Video { prop, .. } => {
                // `/live` reads fall back to the saved value here: the query
                // path has no engine access, and an unset live override equals
                // the saved value anyway.
                let value = match prop {
                    VideoProp::Opacity => cue.video_style().map(|s| serde_json::json!(s.opacity)),
                    VideoProp::Layer => cue.video_style().and_then(|s| s.layer).map(|l| serde_json::json!(l)),
                    VideoProp::BlendMode => cue.video_style().map(|s| serde_json::to_value(s.blend_mode).unwrap_or(serde_json::Value::Null)),
                    VideoProp::Saturation => cue.video_style().map(|s| serde_json::json!(s.saturation)),
                    VideoProp::Scale => cue.video_geometry().map(|g| serde_json::json!(g.scale)),
                    VideoProp::Rotation => cue.video_geometry().map(|g| serde_json::json!(g.rotation)),
                    VideoProp::PanX => cue.video_geometry().map(|g| serde_json::json!(g.pan_x)),
                    VideoProp::PanY => cue.video_geometry().map(|g| serde_json::json!(g.pan_y)),
                };
                Some(value.unwrap_or(serde_json::Value::Null))
            }
        }
    }

    fn reply_data(&self, workspace: &Workspace, src: SocketAddr, port: u16) -> Option<(Uuid, Option<serde_json::Value>)> {
        let instance = workspace.instance_id();
        if self.workspace.is_some_and(|requested| !requested.matches(workspace)) {
            return None;
        }
        Some((instance, self.read(workspace, src, port)))
    }

    /// QLab `/replyFormat`: when the sender set a format, the reply payload is the
    /// format string with `#workspace_id#`, `#address#`, `#status#` and `#data#`
    /// replaced (string data renders bare, everything else as JSON). The reply
    /// address stays `/reply{address}`.
    fn encode_reply_formatted(
        &self,
        workspace_id: Uuid,
        data: Option<serde_json::Value>,
        format: Option<&str>,
    ) -> Option<Vec<u8>> {
        let status = if data.is_some() { "ok" } else { "error" };
        let payload = match format {
            Some(format) if !format.is_empty() => format_reply(format, workspace_id, self.address, status, &data),
            _ => serde_json::json!({ "workspace_id": workspace_id, "address": self.address, "status": status, "data": data }).to_string(),
        };
        rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: format!("/reply{}", self.address),
            args: vec![rosc::OscType::String(payload)],
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

/// Render a `/replyFormat` template: `#data#` is the bare string for string data,
/// compact JSON otherwise, empty when the query errored.
fn format_reply(format: &str, workspace_id: Uuid, address: &str, status: &str, data: &Option<serde_json::Value>) -> String {
    let data_text = match data {
        None => String::new(),
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    };
    format
        .replace("#workspace_id#", &workspace_id.to_string())
        .replace("#address#", address)
        .replace("#status#", status)
        .replace("#data#", &data_text)
}

fn handle_query(query: CueQuery<'_>, src: SocketAddr, handle: &tauri::AppHandle) -> Option<Vec<u8>> {
    let state = handle.try_state::<AppState>()?;
    let port = match state.osc_server.status(false) {
        Ok(OscListenerStatus::Listening { address }) => address.port(),
        _ => 0,
    };
    let (workspace_id, data) = {
        // A read never blocks show work. A busy desk sends no reply; the peer can retry.
        let workspace = state.workspace.try_lock().ok()?;
        query.reply_data(&workspace, src, port)?
    };
    let format = current_reply_format(src);
    query.encode_reply_formatted(workspace_id, data, format.as_deref())
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

    /// A stable fake sender address for query reads (below the ephemeral range).
    fn peer() -> SocketAddr { (Ipv4Addr::LOCALHOST, 9).into() }

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

        let read = |address: &str| CueQuery::parse(&query_message(address)).unwrap().read(&workspace, peer(), 0);
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
        assert_eq!(CueQuery::parse(&query_message(&scoped)).unwrap().read(&workspace, peer(), 0).unwrap().as_array().unwrap().len(), 2);
        let wrong = format!("/workspace/Other/cueLists");
        assert!(CueQuery::parse(&query_message(&wrong)).unwrap().reply_data(&workspace, peer(), 0).is_none());

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
            CueQuery::parse(&query_message(address)).unwrap().read(&workspace, peer(), 0)
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
            CueQuery::parse(&query_message(address)).unwrap().read(workspace, peer(), 0)
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

        let read = |address: &str| CueQuery::parse(&query_message(address)).unwrap().read(&workspace, peer(), 0);
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
            CueQuery::parse(&query_message(path)).unwrap().reply_data(workspace, peer(), 0)
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
            ("/panic", Panic),
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
    fn qlab_full_override_key_set_and_override_window_resolve() {
        use crate::engine::overrides::GlobalOverrides;
        // The whole QLab key set maps to registry keys, in both address styles.
        let one = [rosc::OscType::Int(1)];
        for (address, key, args) in [
            ("/overrides/sysexInputEnabled", "sysex_input", &one[..]),
            ("/overrides/sysexOutputEnabled", "sysex_output", &one[..]),
            ("/overrides/networkExternalOutputEnabled", "network_external_output", &one[..]),
            ("/overrides/networkLocalInputEnabled", "network_local_input", &one[..]),
            ("/overrides/networkLocalOutputEnabled", "network_local_output", &one[..]),
            ("/overrides/timecodeOutputEnabled", "timecode_output", &one[..]),
            ("/overrides/toggleSysexInput", "sysex_input", &[][..]),
            ("/overrides/toggleTimecodeOutput", "timecode_output", &[][..]),
        ] {
            assert!(matches!(resolve_action(address, args), OscAction::SetOverride { key: k, .. } if k == key), "{address}");
        }
        let local = GlobalOverrides::new();
        for key in crate::engine::overrides::OVERRIDE_KEYS {
            assert!(local.get(key).is_some(), "{key}");
        }
        assert_eq!(crate::engine::overrides::OVERRIDE_KEYS.len(), 13);

        // `/overrideWindow {bool}` writes, `/toggleOverrideWindow` toggles,
        // and the no-arg form reads the window state as an any-access query.
        assert!(matches!(resolve_action("/overrideWindow", &[rosc::OscType::Int(1)]),
            OscAction::SetOverrideWindow(Some(true))));
        assert!(matches!(resolve_action("/overrideWindow", &[rosc::OscType::String("no".into())]),
            OscAction::SetOverrideWindow(Some(false))));
        assert!(matches!(resolve_action("/toggleOverrideWindow", &[]), OscAction::SetOverrideWindow(None)));
        assert!(matches!(resolve_action("/overrideWindow", &[]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/toggleOverrideWindow", &[rosc::OscType::Int(1)]), OscAction::Unmatched));
        assert!(matches!(authorized_action("/overrideWindow", &[rosc::OscType::Int(1)], false),
            OscAction::SetOverrideWindow(Some(true))));
        let read_message = query_message("/overrideWindow");
        let read = CueQuery::parse(&read_message).unwrap();
        assert!(matches!(read.kind, CueQueryKind::App(AppQueryKind::OverrideWindow)));
        assert!(read.is_any_access());
        assert!(read.read(&Workspace::new("Window"), peer(), 0).unwrap().is_boolean());
        assert!(parse_text_message(b"/overrideWindow 1").is_some());
        assert!(parse_text_message(b"/toggleOverrideWindow").is_some());
        assert!(parse_text_message(b"/overrideWindow").is_none());
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

        assert!(matches!(resolve_action("/cue/5/preWait", &[rosc::OscType::Double(2.5)]),
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
            ("/cue/5/preWait", &[rosc::OscType::Double(-1.0)][..]),
            ("/cue/5/preWait", &[d(f64::NAN)][..]),
            ("/cue/5/preWait", &[rosc::OscType::Double(1.0), rosc::OscType::Double(2.0)][..]),
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
    fn qlab_notes_color_name_and_flagged_read_and_write_like_qlab() {
        use crate::cue::types::CueColor;
        let s = |v: &str| rosc::OscType::String(v.to_owned());

        // Writes: notes and colorName take one string, flagged takes a boolean
        // (or `toggle` / the /toggle suffix since QLab 5.2).
        assert!(matches!(resolve_action("/cue/5/notes", &[s(" LX 5 is hot")]),
            OscAction::AddressedCue { action: NetworkCueAction::SetNotes(ref n), .. } if n == " LX 5 is hot"));
        assert!(matches!(resolve_action("/cue/5/colorName", &[s("green")]),
            OscAction::AddressedCue { action: NetworkCueAction::SetColor(CueColor::Green), .. }));
        assert!(matches!(resolve_action("/cue/5/colorName", &[s("Hot Pink")]),
            OscAction::AddressedCue { action: NetworkCueAction::SetColor(CueColor::HotPink), .. }));
        assert!(matches!(resolve_action("/cue/5/flagged", &[rosc::OscType::Int(1)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetFlagged(Some(true)), .. }));
        assert!(matches!(resolve_action("/cue/5/flagged", &[s("no")]),
            OscAction::AddressedCue { action: NetworkCueAction::SetFlagged(Some(false)), .. }));
        assert!(matches!(resolve_action("/cue/5/flagged", &[s("toggle")]),
            OscAction::AddressedCue { action: NetworkCueAction::SetFlagged(None), .. }));
        assert!(matches!(resolve_action("/cue/5/flagged/toggle", &[]),
            OscAction::AddressedCue { action: NetworkCueAction::SetFlagged(None), .. }));
        assert!(matches!(resolve_action("/cue/*/colorName", &[s("red")]),
            OscAction::AddressedCuePattern { action: NetworkCueAction::SetColor(CueColor::Red), .. }));
        for (address, args) in [
            ("/cue/5/notes", &[][..]),
            ("/cue/5/notes", &[s(""), s("x")][..]),
            ("/cue/5/colorName", &[s("chartreuse")][..]),
            ("/cue/5/colorName", &[][..]),
            ("/cue/5/flagged", &[s("maybe")][..]),
            ("/cue/5/flagged/toggle", &[rosc::OscType::Int(1)][..]),
            ("/cue/5/flagged/extra", &[][..]),
        ] {
            assert!(matches!(resolve_action(address, args), OscAction::Unmatched), "{address} {args:?}");
        }
        // Writes stay control-level; plain text takes the single argument.
        assert!(matches!(authorized_action("/cue/5/flagged", &[rosc::OscType::Int(1)], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/cue/5/colorName", &[s("red")], true),
            OscAction::AddressedCue { action, .. } if action.is_write()));
        assert!(parse_text_message(b"/cue/5/notes hello").is_some());
        assert!(parse_text_message(b"/cue/5/colorName red").is_some());
        assert!(parse_text_message(b"/cue/5/flagged 1").is_some());
        assert!(parse_text_message(b"/cue/5/flagged/toggle").is_some());
        assert!(parse_text_message(b"/cue/5/notes").is_none());

        // Reads: the three properties query like the rest, by number or ID.
        let mut workspace = Workspace::new("Property reads");
        let mut cue = crate::cue::memo_cue::MemoCue::new();
        cue.set_number(Some("5".into()));
        cue.set_notes("check the rig".into());
        cue.set_color(CueColor::SkyBlue);
        let id = cue.id();
        workspace.active_cue_list_mut().unwrap().push(Box::new(cue));
        workspace.active_cue_list_mut().unwrap().flagged_cues.insert(id);

        let read = |address: &str, workspace: &Workspace| {
            let message = query_message(address);
            CueQuery::parse(&message).unwrap().read(workspace, peer(), 0)
        };
        assert_eq!(read("/cue/5/notes", &workspace), Some(serde_json::json!("check the rig")));
        assert_eq!(read("/cue/5/colorName", &workspace), Some(serde_json::json!("sky_blue")));
        assert_eq!(read("/cue/5/flagged", &workspace), Some(serde_json::json!(true)));
        assert_eq!(read(&format!("/cue_id/{id}/colorName"), &workspace), Some(serde_json::json!("sky_blue")));
        workspace.active_cue_list_mut().unwrap().flagged_cues.remove(&id);
        assert_eq!(read("/cue/5/flagged", &workspace), Some(serde_json::json!(false)));
        // The toggle suffix is a command, not a query.
        assert!(CueQuery::parse(&query_message("/cue/5/flagged/toggle")).is_none());
        assert!(parse_text_message(b"/cue/5/flagged").is_none(), "reads are binary-OSC only");
    }

    #[test]
    fn qlab_increment_decrement_verbs_parse_in_both_qlab_55_forms() {
        use crate::cue::types::ContinueMode;
        let d = |v: f64| rosc::OscType::Double(v);

        // Delta as the argument: `/cue/5/preWait/+ 1`.
        assert!(matches!(resolve_action("/cue/5/preWait/+", &[d(1.0)]),
            OscAction::AddressedCue { action: NetworkCueAction::AdjustPreWait(v), .. } if v == 1.0));
        assert!(matches!(resolve_action("/cue/5/postWait/-", &[d(0.5)]),
            OscAction::AddressedCue { action: NetworkCueAction::AdjustPostWait(v), .. } if v == -0.5));
        assert!(matches!(resolve_action("/cue/5/continueMode/+", &[rosc::OscType::Int(1)]),
            OscAction::AddressedCue { action: NetworkCueAction::AdjustContinueMode(1), .. }));
        // Delta embedded in the address: `/cue/5/preWait/+/2.5` (QLab 5.5+).
        assert!(matches!(resolve_action("/cue/5/preWait/+/2.5", &[]),
            OscAction::AddressedCue { action: NetworkCueAction::AdjustPreWait(v), .. } if v == 2.5));
        assert!(matches!(resolve_action("/cue/5/preWait/-/0.25", &[]),
            OscAction::AddressedCue { action: NetworkCueAction::AdjustPreWait(v), .. } if v == -0.25));
        assert!(matches!(resolve_action("/cue/*/preWait/+", &[d(1.0)]),
            OscAction::AddressedCuePattern { action: NetworkCueAction::AdjustPreWait(1.0), .. }));
        // Invalid forms stay unmatched.
        for (address, args) in [
            ("/cue/5/preWait/+", &[][..]),
            ("/cue/5/preWait/+", &[rosc::OscType::Double(1.0), rosc::OscType::Double(2.0)][..]),
            ("/cue/5/preWait/+/1", &[d(1.0)][..]),
            ("/cue/5/preWait/+/abc", &[][..]),
            ("/cue/5/preWait/+", &[d(f64::NAN)][..]),
            ("/cue/5/preWait/*", &[d(1.0)][..]),
            ("/cue/5/qName/+", &[d(1.0)][..]),
            ("/cue/5/continueMode/+", &[d(1.5)][..]),
        ] {
            assert!(matches!(resolve_action(address, args), OscAction::Unmatched), "{address} {args:?}");
        }
        // Adjustments are writes: control-level, and not queries.
        assert!(matches!(authorized_action("/cue/5/preWait/+", &[d(1.0)], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/cue/5/preWait/+", &[d(1.0)], true),
            OscAction::AddressedCue { action, .. } if action.is_write()));
        assert!(CueQuery::parse(&query_message("/cue/5/preWait/+")).is_none());
        assert!(CueQuery::parse(&query_message("/cue/5/preWait/+/1")).is_none());
        assert!(parse_text_message(b"/cue/5/preWait/+ 1").is_some());
        let _ = ContinueMode::AutoFollow; // enum stays queryable
    }

    #[test]
    fn qlab_show_control_broadcast_listen_ignore_and_event_format_resolve() {
        // Plain and scoped listen/ignore forms, any access level.
        assert!(matches!(resolve_action("/listen", &[]), OscAction::Broadcast { listen: true, event: None }));
        assert!(matches!(resolve_action("/ignore", &[]), OscAction::Broadcast { listen: false, event: None }));
        assert!(matches!(resolve_action("/listen/go", &[]),
            OscAction::Broadcast { listen: true, event: Some(e) } if e == "go"));
        assert!(matches!(resolve_action("/listen/cue/start/number", &[]),
            OscAction::Broadcast { listen: true, event: Some(e) } if e == "cue/start"));
        assert!(matches!(resolve_action("/ignore/panicAll", &[]),
            OscAction::Broadcast { listen: false, event: Some(e) } if e == "panicAll"));
        assert!(matches!(authorized_action("/listen", &[], false), OscAction::Broadcast { .. }));
        // Unknown events/details, extra segments and arguments are rejected.
        for address in ["/listen/bogus", "/listen/go/bogus", "/listen/go/number/extra", "/listen/go/1", "/ignore"] {
            if address == "/ignore" { continue; }
            assert!(matches!(resolve_action(address, &[]), OscAction::Unmatched), "{address}");
        }
        assert!(matches!(resolve_action("/listen/go", &[rosc::OscType::Int(1)]), OscAction::Unmatched));
        assert!(parse_text_message(b"/listen").is_some());
        assert!(parse_text_message(b"/listen/go").is_some());

        // /eventFormat: one string arg writes (empty resets), no arg is the read query.
        assert!(matches!(resolve_action("/eventFormat", &[rosc::OscType::String("/cue/#data#/start".into())]),
            OscAction::SetEventFormat(ref f) if f == "/cue/#data#/start"));
        assert!(matches!(resolve_action("/eventFormat", &[]), OscAction::Unmatched));
        let read_message = query_message("/eventFormat");
        let read = CueQuery::parse(&read_message).unwrap();
        assert!(matches!(read.kind, CueQueryKind::Sender(SenderQuery::EventFormat)));
        assert!(read.is_any_access());
        assert!(parse_text_message(b"/eventFormat /cue/#data#/start").is_some(), "one string arg is fine in plain text");
        assert!(parse_text_message(b"/eventFormat /cue/#data# /start").is_none(), "plain-text args cannot contain spaces");

        // /panic is now its own transport action (broadcasts panicAll).
        assert!(matches!(resolve_action("/panic", &[]), OscAction::WorkspaceTransport { action: NetworkWorkspaceAction::Panic, .. }));
    }

    #[test]
    fn qlab_cue_panic_verbs_and_app_font_and_working_directory_messages_resolve() {
        // Cue-level /panic with an optional non-negative duration, by number,
        // by uniqueID, and on wildcards (no argument there).
        assert!(matches!(resolve_action("/cue/5/panic", &[]),
            OscAction::AddressedCue { action: NetworkCueAction::Panic(None), number: "5", .. }));
        assert!(matches!(resolve_action("/cue/5/panic", &[rosc::OscType::Double(2.5)]),
            OscAction::AddressedCue { action: NetworkCueAction::Panic(Some(v)), .. } if v == 2.5));
        assert!(matches!(resolve_action("/cue/5/panic", &[rosc::OscType::Int(0)]),
            OscAction::AddressedCue { action: NetworkCueAction::Panic(Some(0.0)), .. }));
        for args in [&[rosc::OscType::Double(-1.0)][..], &[rosc::OscType::String("x".into())][..], &[rosc::OscType::Double(1.0), rosc::OscType::Double(2.0)][..]] {
            assert!(matches!(resolve_action("/cue/5/panic", args), OscAction::Unmatched), "{args:?}");
        }
        let id = uuid::Uuid::new_v4().to_string();
        assert!(matches!(resolve_action(&format!("/cue_id/{id}/panic"), &[]),
            OscAction::AddressedCue { action: NetworkCueAction::Panic(None), .. }));
        assert!(matches!(resolve_action("/cue/*/panic", &[]),
            OscAction::AddressedCuePattern { action: NetworkCueAction::Panic(None), .. }));
        assert!(matches!(resolve_action("/cue/*/panic", &[rosc::OscType::Double(1.0)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/cue/playhead/panic", &[]),
            OscAction::AddressedCue { action: NetworkCueAction::Panic(None), number: "playhead", .. }));
        // Panic is a transport action: any access when unscoped, not a write.
        assert!(matches!(authorized_action("/cue/5/panic", &[], false),
            OscAction::AddressedCue { action: NetworkCueAction::Panic(None), .. }));
        assert!(!NetworkCueAction::Panic(None).is_write());
        assert!(parse_text_message(b"/cue/5/panic").is_some());
        assert!(parse_text_message(b"/cue/5/panic 2").is_some());

        // App-level font and working-directory queries: any access, readable.
        let workspace = Workspace::new("Blue");
        let src: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let names_msg = query_message("/fontNames");
        let names = CueQuery::parse(&names_msg).unwrap();
        assert!(matches!(names.kind, CueQueryKind::App(AppQueryKind::FontNames)));
        assert!(names.is_any_access());
        assert!(names.read(&workspace, src, 53000).unwrap().is_array());
        let families_msg = query_message("/fontFamiliesAndStyles");
        let families = CueQuery::parse(&families_msg).unwrap();
        assert!(matches!(families.kind, CueQueryKind::App(AppQueryKind::FontFamiliesAndStyles)));
        assert!(families.read(&workspace, src, 53000).unwrap().is_object());
        let wd_msg = query_message("/workingDirectory");
        let wd = CueQuery::parse(&wd_msg).unwrap();
        assert!(matches!(wd.kind, CueQueryKind::App(AppQueryKind::WorkingDirectory)));
        assert!(wd.read(&workspace, src, 53000).unwrap().is_string());

        // /workingDirectory write: one string arg, control-level only.
        assert!(matches!(
            resolve_action("/workingDirectory", &[rosc::OscType::String("/tmp".into())]),
            OscAction::SetWorkingDirectory(ref p) if p == "/tmp"));
        assert!(matches!(resolve_action("/workingDirectory", &[]), OscAction::Unmatched));
        assert!(matches!(authorized_action("/workingDirectory", &[rosc::OscType::String("/tmp".into())], false),
            OscAction::Unmatched));
        assert!(matches!(authorized_action("/workingDirectory", &[rosc::OscType::String("/tmp".into())], true),
            OscAction::SetWorkingDirectory(_)));
        assert!(parse_text_message(b"/workingDirectory /tmp").is_some());

        // The write validates (existing directory) and tilde expands to home.
        let dir = std::env::temp_dir();
        let set = set_working_directory(dir.to_str().unwrap()).unwrap();
        assert!(set.is_dir());
        assert_eq!(working_directory(&workspace), set);
        assert!(set_working_directory("/definitely/not/a/real/dir/xyz").is_err());
        let home = set_working_directory("~").unwrap();
        assert!(home.is_dir());
        let _ = set_working_directory(dir.to_str().unwrap());
    }

    #[test]
    fn qlab_media_cue_rate_and_level_saved_and_live_forms_resolve_and_read() {
        use crate::cue::audio_cue::AudioCue;
        let d = |v: f64| rosc::OscType::Double(v);
        // Saved and /live writes by number, by id, and on playhead.
        assert!(matches!(resolve_action("/cue/5/rate", &[d(1.5)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetRate { value, live: false }, number: "5", .. } if value == 1.5));
        assert!(matches!(resolve_action("/cue/5/rate/live", &[d(2.0)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetRate { value, live: true }, .. } if value == 2.0));
        assert!(matches!(resolve_action("/cue/5/level/0/1", &[d(-6.0)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetLevel { row: 0, col: 1, db, live: false }, .. } if db == -6.0));
        assert!(matches!(resolve_action("/cue/5/level/2/3/live", &[rosc::OscType::Int(-12)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetLevel { row: 2, col: 3, db, live: true }, .. } if db == -12.0));
        let id = uuid::Uuid::new_v4().to_string();
        assert!(matches!(resolve_action(&format!("/cue_id/{id}/rate"), &[d(0.5)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetRate { value, .. }, .. } if value == 0.5));
        assert!(matches!(resolve_action("/cue/playhead/level/1/0", &[d(0.0)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetLevel { row: 1, col: 0, .. }, number: "playhead", .. }));
        // Invalid forms stay unmatched: non-positive rate, bare rate write,
        // malformed level paths, out-of-range indices, extra segments.
        for (address, args) in [
            ("/cue/5/rate", &[d(0.0)][..]),
            ("/cue/5/rate", &[d(-1.0)][..]),
            ("/cue/5/rate", &[][..]),
            ("/cue/5/rate", &[d(1.0), d(2.0)][..]),
            ("/cue/5/level/0", &[d(0.0)][..]),
            ("/cue/5/level/0/1/2", &[d(0.0)][..]),
            ("/cue/5/level/x/1", &[d(0.0)][..]),
            ("/cue/5/level/200/1", &[d(0.0)][..]),
            ("/cue/5/level/0/1/live/extra", &[d(0.0)][..]),
            ("/cue/5/level/0/1", &[][..]),
        ] {
            assert!(matches!(resolve_action(address, args), OscAction::Unmatched), "{address} {args:?}");
        }
        // Wildcards accept the property writes (control-level), never reads.
        assert!(matches!(resolve_action("/cue/*/rate", &[d(1.0)]),
            OscAction::AddressedCuePattern { action: NetworkCueAction::SetRate { .. }, .. }));
        assert!(matches!(resolve_action("/cue/*/level/0/0", &[d(0.0)]),
            OscAction::AddressedCuePattern { action: NetworkCueAction::SetLevel { .. }, .. }));
        assert!(matches!(resolve_action("/cue/*/rate", &[]), OscAction::Unmatched));
        // Property writes need the allowlist (control level), live included.
        assert!(matches!(authorized_action("/cue/5/rate", &[d(1.0)], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/cue/5/level/0/0", &[d(0.0)], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/cue/5/rate/live", &[d(1.0)], true),
            OscAction::AddressedCue { action: NetworkCueAction::SetRate { live: true, .. }, .. }));
        assert!(NetworkCueAction::SetRate { value: 1.0, live: true }.is_write());
        assert!(parse_text_message(b"/cue/5/level/0/1 -6").is_some());
        assert!(parse_text_message(b"/cue/5/rate 1.5").is_some());

        // Queries: saved and /live reads resolve and return the cue's values.
        let mut workspace = Workspace::new("Levels");
        let mut cue = AudioCue::new();
        cue.set_number(Some("7".into()));
        cue.set_rate(1.25);
        cue.set_level(0, 1, -6.0);
        workspace.active_cue_list_mut().unwrap().push(Box::new(cue));
        let src: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let read = |address: &str| {
            let message = query_message(address);
            CueQuery::parse(&message).unwrap().read(&workspace, src, 53000)
        };
        assert_eq!(read("/cue/7/rate"), Some(serde_json::json!(1.25)));
        assert_eq!(read("/cue/7/rate/live"), Some(serde_json::json!(1.25)));
        assert_eq!(read("/cue/7/level/0/1"), Some(serde_json::json!(-6.0)));
        assert_eq!(read("/cue/7/level/1/1"), Some(serde_json::Value::Null), "unset crosspoint reads null");
        assert_eq!(read("/cue/7/level/1/1/live"), Some(serde_json::Value::Null));
        let rate_msg = query_message("/cue/7/rate/live");
        let rate_q = CueQuery::parse(&rate_msg).unwrap();
        assert!(matches!(rate_q.kind, CueQueryKind::Cue { property: CueQueryProperty::Rate { live: true }, .. }));
        let level_msg = query_message("/cue/7/level/0/1/live");
        let level_q = CueQuery::parse(&level_msg).unwrap();
        assert!(matches!(level_q.kind, CueQueryKind::Cue {
            property: CueQueryProperty::Level { row: 0, col: 1, live: true }, .. }));
        assert!(CueQuery::parse(&query_message("/cue/7/level/0")).is_none());
        assert!(CueQuery::parse(&query_message("/cue/7/level/0/1/2")).is_none());
        // Non-media cues report null for rate/level.
        let mut memo_workspace = Workspace::new("Memo");
        let mut memo = crate::cue::memo_cue::MemoCue::new();
        memo.set_number(Some("1".into()));
        memo_workspace.active_cue_list_mut().unwrap().push(Box::new(memo));
        let memo_msg = query_message("/cue/1/rate");
        let memo_read = CueQuery::parse(&memo_msg).unwrap().read(&memo_workspace, src, 53000);
        assert_eq!(memo_read, Some(serde_json::Value::Null));
    }

    #[test]
    fn qlab_video_cue_properties_saved_live_and_queries_resolve() {
        use crate::cue::video_cue::VideoCue;
        let d = |v: f64| rosc::OscType::Double(v);
        let s = |text: &str| rosc::OscType::String(text.into());
        // Saved and /live writes for each video property.
        assert!(matches!(resolve_action("/cue/5/opacity", &[d(0.5)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetVideoProp { prop: VideoProp::Opacity, value: VideoPropValue::Num(v), live: false }, .. } if v == 0.5));
        assert!(matches!(resolve_action("/cue/5/opacity/live", &[d(0.25)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetVideoProp { prop: VideoProp::Opacity, live: true, .. }, .. }));
        assert!(matches!(resolve_action("/cue/5/layer", &[rosc::OscType::Int(10)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetVideoProp { prop: VideoProp::Layer, value: VideoPropValue::Num(10.0), .. }, .. }));
        assert!(matches!(resolve_action("/cue/5/blendMode", &[s("multiply")]),
            OscAction::AddressedCue { action: NetworkCueAction::SetVideoProp { prop: VideoProp::BlendMode, value: VideoPropValue::Text(ref n), .. }, .. } if n == "multiply"));
        assert!(matches!(resolve_action("/cue/5/saturation", &[d(1.5)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetVideoProp { prop: VideoProp::Saturation, .. }, .. }));
        assert!(matches!(resolve_action("/cue/5/scale", &[d(2.0)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetVideoProp { prop: VideoProp::Scale, value: VideoPropValue::Num(2.0), .. }, .. }));
        assert!(matches!(resolve_action("/cue/5/rotation", &[d(90.0)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetVideoProp { prop: VideoProp::Rotation, .. }, .. }));
        assert!(matches!(resolve_action("/cue/5/translateX", &[d(-0.5)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetVideoProp { prop: VideoProp::PanX, .. }, .. }));
        assert!(matches!(resolve_action("/cue/5/translateY/live", &[d(0.5)]),
            OscAction::AddressedCue { action: NetworkCueAction::SetVideoProp { prop: VideoProp::PanY, live: true, .. }, .. }));
        // Out-of-range values, unknown blend modes, bare and multi-arg forms.
        for (address, args) in [
            ("/cue/5/opacity", &[d(1.5)][..]),
            ("/cue/5/opacity", &[d(-0.1)][..]),
            ("/cue/5/layer", &[d(0.0)][..]),
            ("/cue/5/layer", &[d(1001.0)][..]),
            ("/cue/5/scale", &[d(0.0)][..]),
            ("/cue/5/rotation", &[d(400.0)][..]),
            ("/cue/5/translateX", &[d(2.0)][..]),
            ("/cue/5/blendMode", &[s("bogus")][..]),
            ("/cue/5/blendMode", &[d(1.0)][..]),
            ("/cue/5/opacity", &[][..]),
            ("/cue/5/opacity", &[d(1.0), d(2.0)][..]),
        ] {
            assert!(matches!(resolve_action(address, args), OscAction::Unmatched), "{address} {args:?}");
        }
        // Wildcards accept video property writes; control-level only.
        assert!(matches!(resolve_action("/cue/*/opacity", &[d(0.5)]),
            OscAction::AddressedCuePattern { action: NetworkCueAction::SetVideoProp { prop: VideoProp::Opacity, .. }, .. }));
        assert!(matches!(authorized_action("/cue/5/opacity", &[d(0.5)], false), OscAction::Unmatched));
        assert!(matches!(authorized_action("/cue/5/opacity/live", &[d(0.5)], true),
            OscAction::AddressedCue { action: NetworkCueAction::SetVideoProp { live: true, .. }, .. }));
        assert!(parse_text_message(b"/cue/5/opacity 0.5").is_some());
        assert!(parse_text_message(b"/cue/5/blendMode multiply").is_some());

        // Blend mode names normalize across spellings.
        assert_eq!(crate::engine::output_engine::BlendMode::from_osc_name("Soft Light"),
            Some(crate::engine::output_engine::BlendMode::SoftLight));
        assert_eq!(crate::engine::output_engine::BlendMode::from_osc_name("source-atop"),
            Some(crate::engine::output_engine::BlendMode::SourceAtop));
        assert_eq!(crate::engine::output_engine::BlendMode::from_osc_name("nope"), None);

        // Queries read the video cue's saved style/geometry.
        let mut workspace = Workspace::new("Video");
        let mut cue = VideoCue::new();
        cue.set_number(Some("9".into()));
        workspace.active_cue_list_mut().unwrap().push(Box::new(cue));
        let src: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let read = |address: &str| {
            let message = query_message(address);
            CueQuery::parse(&message).unwrap().read(&workspace, src, 53000)
        };
        assert_eq!(read("/cue/9/opacity"), Some(serde_json::json!(1.0)));
        assert_eq!(read("/cue/9/opacity/live"), Some(serde_json::json!(1.0)));
        assert_eq!(read("/cue/9/blendMode"), Some(serde_json::json!("normal")));
        assert_eq!(read("/cue/9/saturation"), Some(serde_json::json!(1.0)));
        assert_eq!(read("/cue/9/scale"), Some(serde_json::json!(1.0)));
        assert_eq!(read("/cue/9/rotation"), Some(serde_json::json!(0)));
        assert_eq!(read("/cue/9/translateX"), Some(serde_json::json!(0.0)));
        assert_eq!(read("/cue/9/layer"), Some(serde_json::Value::Null), "automatic layer reads null");
        let opacity_msg = query_message("/cue/9/opacity/live");
        let opacity_q = CueQuery::parse(&opacity_msg).unwrap();
        assert!(matches!(opacity_q.kind, CueQueryKind::Cue {
            property: CueQueryProperty::Video { prop: VideoProp::Opacity, live: true }, .. }));
        assert!(CueQuery::parse(&query_message("/cue/9/opacity/live/extra")).is_none());
    }

    #[test]
    fn listen_subscriptions_receive_qlab_event_workspace_messages_over_udp() {
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let protocol = Protocol::Osc;
        let config = OscReceiveConfig { enabled: true, port: 0, ..OscReceiveConfig::default() };
        let listener = Listener::spawn(
            protocol, protocol.settings(&config), || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            |packet, _allowed, src| {
                // Mirror the production dispatch for broadcast subscriptions.
                if let rosc::OscPacket::Message(message) = packet {
                    if let OscAction::Broadcast { listen, event } = resolve_action(&message.addr, &message.args) {
                        if listen {
                            crate::engine::osc_broadcast::listen(src, event.as_deref());
                        } else {
                            crate::engine::osc_broadcast::ignore(src, event.as_deref());
                        }
                    }
                }
            },
            |_query, _src| None,
        );
        let OscListenerStatus::Listening { address } = statuses.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected an OSC socket");
        };
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        sender.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let src = sender.local_addr().unwrap();
        let mut response = [0; 4096];
        let send = |addr: &str| {
            sender.send_to(&query_packet(addr), address).unwrap();
        };
        let recv = |response: &mut [u8; 4096]| -> rosc::OscMessage {
            let (length, _origin) = sender.recv_from(response).unwrap();
            let (_, rosc::OscPacket::Message(message)) = rosc::decoder::decode_udp(&response[..length]).unwrap() else {
                panic!("expected a single OSC message");
            };
            message
        };

        // Subscribe to all events, then fire a go event with cue arguments.
        // /listen has no acknowledgement, so fire until the subscription has
        // registered: a firing before registration sends nothing, so the
        // first packet received is always the multi-argument form.
        send("/listen");
        let workspace_id = uuid::Uuid::new_v4();
        let mut cue = crate::cue::memo_cue::MemoCue::new();
        cue.set_number(Some("24".into()));
        cue.set_name("intro music".into());
        let cue_id = cue.id();
        let cue_args = || crate::engine::osc_broadcast::CueEventArgs {
            number: cue.number().unwrap_or(""),
            name: cue.name(),
            unique_id: cue.id(),
            cue_type: cue.cue_type().qlab_name(),
        };
        sender.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let multi = loop {
            crate::engine::osc_broadcast::broadcast(workspace_id, "go", Some(cue_args()));
            if let Ok((length, _)) = sender.recv_from(&mut response) {
                let (_, rosc::OscPacket::Message(message)) = rosc::decoder::decode_udp(&response[..length]).unwrap() else {
                    panic!("expected a single OSC message");
                };
                break message;
            }
        };
        sender.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        // Default form: the multi-arg message plus each single-argument detail.
        assert_eq!(multi.addr, "/qlab/event/workspace/go");
        assert_eq!(multi.args, vec![
            rosc::OscType::String("24".into()),
            rosc::OscType::String("intro music".into()),
            rosc::OscType::String(cue_id.to_string()),
            rosc::OscType::String("Memo".into()),
        ]);
        assert_eq!(recv(&mut response).addr, "/qlab/event/workspace/go/number");
        assert_eq!(recv(&mut response).addr, "/qlab/event/workspace/go/name");
        assert_eq!(recv(&mut response).addr, "/qlab/event/workspace/go/uniqueID");
        let type_msg = recv(&mut response);
        assert_eq!(type_msg.addr, "/qlab/event/workspace/go/type");
        assert_eq!(type_msg.args, vec![rosc::OscType::String("Memo".into())]);

        // A bare event (no cue args) arrives without arguments.
        crate::engine::osc_broadcast::broadcast(workspace_id, "panicAll", None);
        let bare = recv(&mut response);
        assert_eq!(bare.addr, "/qlab/event/workspace/panicAll");
        assert!(bare.args.is_empty());

        // /ignore stops delivery. Keep broadcasting until the socket goes
        // quiet: once the registry drops this peer no further packets are
        // sent, so a timeout means /ignore was processed.
        send("/ignore");
        sender.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        loop {
            crate::engine::osc_broadcast::broadcast(workspace_id, "go", None);
            if sender.recv_from(&mut response).is_err() {
                break;
            }
        }
        assert!(!crate::engine::osc_broadcast::is_listening(src), "ignored listeners get nothing");
        listener.stop();
    }

    #[test]
    fn slip_codec_round_trips_and_decodes_split_feeds() {
        let payloads: Vec<Vec<u8>> = vec![
            b"/version".to_vec(),
            vec![0xC0, 0xDB, 0x00, 0xFF, 0xC0, 0xDB],
            query_packet("/cue/1/number"),
        ];
        let mut wire = Vec::new();
        for payload in &payloads {
            wire.extend_from_slice(&slip_encode(payload));
        }
        // Every byte of the escape alphabet survives the round trip.
        assert!(wire.windows(2).any(|w| w == [0xDB, 0xDC]));
        assert!(wire.windows(2).any(|w| w == [0xDB, 0xDD]));
        let mut decoder = SlipDecoder::new();
        let mut got = Vec::new();
        for chunk in wire.chunks(3) {
            got.extend(decoder.feed(chunk));
        }
        assert_eq!(got, payloads);
    }

    #[test]
    fn osc_tcp_endpoint_answers_queries_and_always_reply_over_slip() {
        use std::io::{Read, Write};
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let (packet_tx, packets) = crossbeam_channel::unbounded();
        let config = ListenerConfig { enabled: true, port: 0, allowed_ips: vec![] };
        let endpoint = TcpEndpoint::spawn(
            config,
            || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            move |packet, _allowed, _src| { let _ = packet_tx.send(packet.clone()); },
            |query, _src| query.encode_reply_formatted(uuid::Uuid::new_v4(), Some(serde_json::json!("ok-data")), None),
        );
        let OscListenerStatus::Listening { address } = statuses.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected a TCP listener");
        };
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let peer = stream.local_addr().unwrap();
        let send = |stream: &mut std::net::TcpStream, packet: &[u8]| {
            stream.write_all(&slip_encode(packet)).unwrap();
        };
        let recv = |stream: &mut std::net::TcpStream| -> rosc::OscMessage {
            let mut decoder = SlipDecoder::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0, "connection closed");
                if let Some(payload) = decoder.feed(&buf[..n]).into_iter().next() {
                    let (_, rosc::OscPacket::Message(message)) = rosc::decoder::decode_udp(&payload).unwrap() else {
                        panic!("expected a single OSC message");
                    };
                    return message;
                }
            }
        };

        // An app-level query over TCP gets a SLIP-framed reply on the same connection.
        send(&mut stream, &query_packet("/version"));
        let reply = recv(&mut stream);
        assert_eq!(reply.addr, "/reply/version");
        let [rosc::OscType::String(json)] = reply.args.as_slice() else { panic!("expected JSON data") };
        assert_eq!(serde_json::from_str::<serde_json::Value>(json).unwrap()["data"], "ok-data");

        // alwaysReply works over TCP and commands still reach on_packet.
        send(&mut stream, &rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/alwaysReply".into(),
            args: vec![rosc::OscType::Int(1)],
        })).unwrap());
        assert_eq!(recv(&mut stream).addr, "/reply/alwaysReply");
        send(&mut stream, &rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/thump".into(),
            args: vec![],
        })).unwrap());
        assert_eq!(recv(&mut stream).addr, "/reply/thump");
        // Both commands reach on_packet, in order: /alwaysReply then /thump.
        let first = packets.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = packets.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(first, rosc::OscPacket::Message(ref m) if m.addr == "/alwaysReply"));
        assert!(matches!(second, rosc::OscPacket::Message(ref m) if m.addr == "/thump"));

        // The peer is pinned while connected (QLab never expires TCP clients)...
        assert!(crate::engine::osc_updates::is_pinned(peer));
        assert!(tcp_connection(peer).is_some());
        // ...and fully disconnected when the stream closes.
        drop(stream);
        let deadline = Instant::now() + Duration::from_secs(2);
        while tcp_connection(peer).is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(tcp_connection(peer).is_none(), "the connection is unregistered on close");
        assert!(!crate::engine::osc_updates::is_pinned(peer), "the pin is released on close");
        endpoint.stop();
    }

    #[test]
    fn qlab_cue_dictionaries_and_children_queries_match_the_qlab_shape() {
        use crate::cue::{group_cue::GroupCue, memo_cue::MemoCue, wait_cue::WaitCue};
        let mut workspace = Workspace::new("Dict reads");
        let mut child = MemoCue::new();
        child.set_number(Some("2.1".into()));
        child.set_name("child memo".into());
        let child_id = child.id();
        let mut group = GroupCue::new();
        group.set_number(Some("2".into()));
        group.set_name("scene".into());
        group.children.push(Box::new(child));
        let group_id = group.id();
        let mut wait = WaitCue::new();
        wait.set_number(Some("3".into()));
        let wait_id = wait.id();
        let list = workspace.active_cue_list_mut().unwrap();
        list.push(Box::new(group));
        list.push(Box::new(wait));
        list.flagged_cues.insert(group_id);

        let read = |address: &str, workspace: &Workspace| {
            let message = query_message(address);
            CueQuery::parse(&message).unwrap().read(workspace, peer(), 0)
        };
        // /cueLists carries QLab cue-list dictionaries with nested cue dicts.
        let lists = read("/cueLists", &workspace).unwrap();
        let lists = lists.as_array().unwrap();
        assert_eq!(lists.len(), 1);
        let list = &lists[0];
        assert_eq!(list["uniqueID"], list["id"]);
        assert_eq!(list["type"], "Cue List");
        assert_eq!(list["listName"], "Cue List 1");
        let cues = list["cues"].as_array().unwrap();
        assert_eq!(cues.len(), 2);
        let group_dict = &cues[0];
        assert_eq!(group_dict["number"], "2");
        assert_eq!(group_dict["name"], "scene");
        assert_eq!(group_dict["type"], "Group");
        assert_eq!(group_dict["flagged"], true);
        assert_eq!(group_dict["armed"], true);
        assert!(group_dict["colorName"].is_string(), "groups carry their palette color");
        assert_eq!(group_dict["uniqueID"], group_id.to_string());
        assert_eq!(group_dict["cues"].as_array().unwrap().len(), 1);
        assert_eq!(group_dict["cues"][0]["number"], "2.1");
        assert_eq!(group_dict["cues"][0]["type"], "Memo");
        assert_eq!(cues[1]["type"], "Wait");
        assert_eq!(cues[1]["cues"].as_array().unwrap().len(), 0);

        // /cue/{n}/children and /cue_id/{id}/children return child dicts.
        let children = read("/cue/2/children", &workspace).unwrap();
        let children = children.as_array().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0]["uniqueID"], child_id.to_string());
        assert_eq!(children[0]["listName"], "Cue List 1");
        let by_id = read(&format!("/cue_id/{wait_id}/children"), &workspace).unwrap();
        assert_eq!(by_id.as_array().unwrap().len(), 0, "a plain cue has no children");
        // children is a query, not a writable verb.
        assert!(matches!(resolve_action("/cue/2/children", &[rosc::OscType::Int(1)]), OscAction::Unmatched));
        assert!(parse_text_message(b"/cue/2/children").is_none(), "reads are binary-OSC only");
    }

    #[test]
    fn qlab_workspace_save_undo_redo_and_toggle_pause_are_control_level_transport() {
        use NetworkWorkspaceAction as Transport;
        for (address, expected) in [
            ("/save", Transport::Save),
            ("/undo", Transport::Undo),
            ("/redo", Transport::Redo),
            ("/togglePause", Transport::TogglePaused),
        ] {
            assert!(matches!(resolve_action(address, &[]), OscAction::WorkspaceTransport { action, .. } if action == expected), "{address}");
            assert!(matches!(resolve_action(&format!("/workspace/Blue{address}"), &[]),
                OscAction::WorkspaceTransport { action, .. } if action == expected), "scoped {address}");
            // Control-level like the other workspace transport verbs.
            assert!(matches!(authorized_action(address, &[], false), OscAction::Unmatched), "{address}");
            assert!(matches!(authorized_action(address, &[], true), OscAction::WorkspaceTransport { .. }), "{address}");
            // No arguments allowed; plain text works.
            assert!(matches!(resolve_action(address, &[rosc::OscType::Int(1)]), OscAction::Unmatched), "{address}");
            assert!(parse_text_message(address.as_bytes()).is_some(), "{address}");
        }
    }

    #[test]
    fn qlab_cue_action_verbs_hard_stop_toggle_pause_devamp_and_load_resolve() {
        use NetworkPauseAction::Toggle;
        let d = |v: f64| rosc::OscType::Double(v);
        for (address, expected) in [
            ("/cue/5/hardStop", NetworkCueAction::HardStop),
            ("/cue/5/devamp", NetworkCueAction::Devamp),
            ("/cue/5/togglePause", NetworkCueAction::SetPause(Toggle)),
            ("/cue/playhead/hardStop", NetworkCueAction::HardStop),
            ("/cue/selected/togglePause", NetworkCueAction::SetPause(Toggle)),
        ] {
            assert!(matches!(resolve_action(address, &[]), OscAction::AddressedCue { action, .. } if action == expected), "{address}");
        }
        assert!(matches!(resolve_action("/cue/5/load", &[]),
            OscAction::AddressedCue { action: NetworkCueAction::Load(v), .. } if v == 0.0));
        assert!(matches!(resolve_action("/cue/5/load", &[d(12.5)]),
            OscAction::AddressedCue { action: NetworkCueAction::Load(v), .. } if v == 12.5));
        // Negative loads count from the end of the cue.
        assert!(matches!(resolve_action("/cue/5/load", &[d(-3.0)]),
            OscAction::AddressedCue { action: NetworkCueAction::Load(v), .. } if v == -3.0));
        let uuid = uuid::Uuid::new_v4();
        assert!(matches!(resolve_action(&format!("/cue_id/{uuid}/hardStop"), &[]),
            OscAction::AddressedCue { action: NetworkCueAction::HardStop, .. }));
        assert!(matches!(resolve_action(&format!("/cue_id/{uuid}/load"), &[d(2.0)]),
            OscAction::AddressedCue { action: NetworkCueAction::Load(2.0), .. }));
        // Wildcards take the stop-family verbs, not devamp/load.
        assert!(matches!(resolve_action("/cue/*/hardStop", &[]),
            OscAction::AddressedCuePattern { action: NetworkCueAction::HardStop, .. }));
        assert!(matches!(resolve_action("/cue/*/togglePause", &[]),
            OscAction::AddressedCuePattern { action: NetworkCueAction::SetPause(Toggle), .. }));
        assert!(matches!(resolve_action("/cue/*/devamp", &[]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/cue/*/load", &[rosc::OscType::Double(1.0)]), OscAction::Unmatched));
        // Arguments on no-arg verbs and malformed load args are rejected.
        assert!(matches!(resolve_action("/cue/5/hardStop", &[rosc::OscType::Double(1.0)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/cue/5/devamp", &[rosc::OscType::Double(1.0)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/cue/5/load", &[d(f64::NAN)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/cue/5/load", &[d(1.0), d(2.0)]), OscAction::Unmatched));
        // Actions are not saved-field writes: any access level, like start/stop.
        assert!(matches!(authorized_action("/cue/5/hardStop", &[], false),
            OscAction::AddressedCue { action: NetworkCueAction::HardStop, .. }));
        assert!(matches!(authorized_action("/cue/5/load", &[d(1.0)], false),
            OscAction::AddressedCue { action, .. } if !action.is_write()));
        assert!(!NetworkCueAction::Load(1.0).is_write());
        assert!(!NetworkCueAction::HardStop.is_write());
        assert!(!NetworkCueAction::Devamp.is_write());
        assert!(parse_text_message(b"/cue/5/hardStop").is_some());
        assert!(parse_text_message(b"/cue/5/togglePause").is_some());
        assert!(parse_text_message(b"/cue/5/devamp").is_some());
        assert!(parse_text_message(b"/cue/5/load 2.5").is_some());
        assert!(parse_text_message(b"/cue/5/load").is_some());
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
    fn qlab_updates_subscription_resolves_at_any_access_level() {
        assert!(matches!(resolve_action("/updates", &[rosc::OscType::Int(1)]), OscAction::Updates(true)));
        assert!(matches!(resolve_action("/updates", &[rosc::OscType::String("no".into())]), OscAction::Updates(false)));
        assert!(matches!(resolve_action("/workspace/Blue/updates", &[rosc::OscType::Double(1.0)]), OscAction::Updates(true)));
        // Any access level: not gated by the allowlist, unlike transport/writes.
        assert!(matches!(authorized_action("/updates", &[rosc::OscType::Int(1)], false), OscAction::Updates(true)));
        assert!(matches!(resolve_action("/updates", &[]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/updates", &[rosc::OscType::String("maybe".into())]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/updates", &[rosc::OscType::Int(1), rosc::OscType::Int(0)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/updates/extra", &[rosc::OscType::Int(1)]), OscAction::Unmatched));
        assert!(parse_text_message(b"/updates 1").is_some());
        assert!(parse_text_message(b"/updates").is_none());
    }

    #[test]
    fn qlab_always_reply_resolves_at_any_access_level() {
        assert!(matches!(resolve_action("/alwaysReply", &[rosc::OscType::Int(1)]), OscAction::AlwaysReply(true)));
        assert!(matches!(resolve_action("/alwaysReply", &[rosc::OscType::String("no".into())]), OscAction::AlwaysReply(false)));
        assert!(matches!(resolve_action("/alwaysReply", &[rosc::OscType::Bool(false)]), OscAction::AlwaysReply(false)));
        assert!(matches!(resolve_action("/workspace/Blue/alwaysReply", &[rosc::OscType::Double(1.0)]), OscAction::AlwaysReply(true)));
        // Any access level: not gated by the allowlist, unlike transport/writes.
        assert!(matches!(authorized_action("/alwaysReply", &[rosc::OscType::Int(1)], false), OscAction::AlwaysReply(true)));
        assert!(matches!(authorized_action("/workspace/Blue/alwaysReply", &[rosc::OscType::Int(0)], false), OscAction::AlwaysReply(false)));
        assert!(matches!(resolve_action("/alwaysReply", &[]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/alwaysReply", &[rosc::OscType::String("maybe".into())]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/alwaysReply", &[rosc::OscType::Int(1), rosc::OscType::Int(0)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/alwaysReply/extra", &[rosc::OscType::Int(1)]), OscAction::Unmatched));
        let parsed = parse_text_message(b"/alwaysReply 1").unwrap();
        assert!(matches!(resolve_action(&parsed.addr, &parsed.args), OscAction::AlwaysReply(true)));
        assert!(parse_text_message(b"/alwaysReply true").is_some());
        assert!(parse_text_message(b"/alwaysReply").is_none());
        assert!(parse_text_message(b"/alwaysReply 1 0").is_none());
    }

    #[test]
    fn always_reply_registry_tracks_senders_and_expires_after_the_keepalive_window() {
        // Ports below the ephemeral range never collide with real sockets.
        let peer: SocketAddr = (Ipv4Addr::LOCALHOST, 9).into();
        let stale: SocketAddr = (Ipv4Addr::LOCALHOST, 10).into();
        set_always_reply(peer, true);
        assert!(always_reply_enabled(peer));
        assert!(!always_reply_enabled(stale));
        touch_always_reply(peer);
        assert!(always_reply_enabled(peer));
        set_always_reply(peer, false);
        assert!(!always_reply_enabled(peer));
        // Entries idle longer than the 61 s keepalive are pruned on read.
        set_always_reply(stale, true);
        if let Ok(mut guard) = ALWAYS_REPLY.lock() {
            let old = Instant::now().checked_sub(Duration::from_secs(120)).unwrap();
            guard.as_mut().unwrap().insert(stale, old);
        }
        assert!(!always_reply_enabled(stale));
        if let Ok(guard) = ALWAYS_REPLY.lock() {
            assert!(!guard.as_ref().unwrap().contains_key(&stale));
        }
    }

    #[test]
    fn always_reply_clients_get_a_reply_for_every_message_until_they_opt_out() {
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let protocol = Protocol::Osc;
        let config = OscReceiveConfig {
            enabled: true,
            port: 0,
            allowed_ips: vec!["127.0.0.1".into()],
            ..OscReceiveConfig::default()
        };
        let listener = Listener::spawn(
            protocol, protocol.settings(&config), || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            |_, _, _| {},
            |_query, _src| Some(rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
                addr: "/reply/cue/101/number".into(),
                args: vec![rosc::OscType::String("canned".into())],
            })).unwrap()),
        );
        let OscListenerStatus::Listening { address } = statuses.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected an OSC socket");
        };
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        sender.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut response = [0; 1024];
        let send = |addr: &str, args: Vec<rosc::OscType>| {
            let packet = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
                addr: addr.into(), args,
            })).unwrap();
            sender.send_to(&packet, address).unwrap();
        };
        let recv_reply = |response: &mut [u8; 1024]| -> (String, String) {
            let (length, origin) = sender.recv_from(response).unwrap();
            assert_eq!(origin, address);
            let (remaining, rosc::OscPacket::Message(message)) = rosc::decoder::decode_udp(&response[..length]).unwrap() else {
                panic!("expected a single OSC reply");
            };
            assert!(remaining.is_empty());
            let [rosc::OscType::String(json)] = message.args.as_slice() else { panic!("expected JSON data") };
            (message.addr, json.clone())
        };
        let status_of = |json: &str| serde_json::from_str::<serde_json::Value>(json).unwrap()["status"].clone();

        // Without opting in, no replies are sent.
        send("/thump", vec![]);
        sender.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        assert!(sender.recv_from(&mut response).is_err());
        sender.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        // `/alwaysReply 1` registers the sender and is itself acknowledged.
        send("/alwaysReply", vec![rosc::OscType::Int(1)]);
        let (addr, json) = recv_reply(&mut response);
        assert_eq!(addr, "/reply/alwaysReply");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            serde_json::json!({"address": "/alwaysReply", "status": "ok"}));

        // Every following message gets a reply; unmatched ones report an error.
        send("/go", vec![]);
        let (addr, json) = recv_reply(&mut response);
        assert_eq!(addr, "/reply/go");
        assert_eq!(status_of(&json), "ok");
        send("/bogus", vec![]);
        let (addr, json) = recv_reply(&mut response);
        assert_eq!(addr, "/reply/bogus");
        assert_eq!(status_of(&json), "error");

        // Queries keep their dedicated reply and are not answered twice.
        send("/cue/101/number", vec![]);
        let (addr, json) = recv_reply(&mut response);
        assert_eq!(addr, "/reply/cue/101/number");
        assert_eq!(json, "canned");
        sender.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        assert!(sender.recv_from(&mut response).is_err(), "a query must not also get an alwaysReply packet");
        sender.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        // `/alwaysReply 0` unregisters the sender: no further replies.
        send("/alwaysReply", vec![rosc::OscType::Int(0)]);
        send("/panic", vec![]);
        sender.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        assert!(sender.recv_from(&mut response).is_err());
        listener.stop();
    }

    #[test]
    fn app_level_queries_report_version_and_open_workspaces() {
        let workspace = Workspace::new("App query rehearsal");
        let instance = workspace.instance_id();

        let version_message = query_message("/version");
        let version = CueQuery::parse(&version_message).unwrap();
        assert!(matches!(version.kind, CueQueryKind::App(AppQueryKind::Version)));
        assert_eq!(version.read(&workspace, peer(), 0),
            Some(serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string())));

        let workspaces_message = query_message("/workspaces");
        let workspaces = CueQuery::parse(&workspaces_message).unwrap();
        assert!(matches!(workspaces.kind, CueQueryKind::App(AppQueryKind::Workspaces)));
        // QLab shape: an array of {uniqueID, displayName, port, udpReplyPort, version}.
        // Inkue replies to the requesting socket, so udpReplyPort is the sender's port.
        assert_eq!(workspaces.read(&workspace, peer(), 53000), Some(serde_json::json!([{
            "uniqueID": instance.to_string(),
            "displayName": "App query rehearsal",
            "port": 53000,
            "udpReplyPort": 9,
            "version": env!("CARGO_PKG_VERSION"),
        }])));

        // App-level queries are never workspace-scoped, take no arguments,
        // and are not accepted on the plain-text command port.
        assert!(CueQuery::parse(&query_message("/workspace/Blue/version")).is_none());
        assert!(CueQuery::parse(&query_message("/workspace/Blue/workspaces")).is_none());
        assert!(CueQuery::parse(&query_message("/version/extra")).is_none());
        let mut with_arg = query_message("/workspaces");
        with_arg.args.push(rosc::OscType::Int(1));
        assert!(CueQuery::parse(&with_arg).is_none());
        assert!(parse_text_message(b"/version").is_none());
        assert!(parse_text_message(b"/workspaces").is_none());

        // Replies keep the standard query envelope at /reply{address}.
        let bytes = version.encode_reply_formatted(instance, version.read(&workspace, peer(), 0), None).unwrap();
        let (remaining, rosc::OscPacket::Message(message)) = rosc::decoder::decode_udp(&bytes).unwrap() else {
            panic!("expected a single OSC reply");
        };
        assert!(remaining.is_empty());
        assert_eq!(message.addr, "/reply/version");
        let [rosc::OscType::String(json)] = message.args.as_slice() else { panic!("expected JSON data") };
        assert_eq!(serde_json::from_str::<serde_json::Value>(json).unwrap(), serde_json::json!({
            "workspace_id": instance, "address": "/version", "status": "ok",
            "data": env!("CARGO_PKG_VERSION"),
        }));
    }

    #[test]
    fn always_reply_and_updates_have_per_sender_read_forms() {
        let workspace = Workspace::new("Sender status");
        // Port 11: the registry TTL test owns ports 9/10 in the shared map.
        let sender_peer: SocketAddr = (Ipv4Addr::LOCALHOST, 11).into();

        let ar_message = query_message("/alwaysReply");
        let ar = CueQuery::parse(&ar_message).unwrap();
        assert!(matches!(ar.kind, CueQueryKind::Sender(SenderQuery::AlwaysReply)));
        assert_eq!(ar.read(&workspace, sender_peer, 0), Some(serde_json::json!(false)));
        set_always_reply(sender_peer, true);
        assert_eq!(ar.read(&workspace, sender_peer, 0), Some(serde_json::json!(true)));
        set_always_reply(sender_peer, false);
        assert_eq!(ar.read(&workspace, sender_peer, 0), Some(serde_json::json!(false)));

        let up_message = query_message("/updates");
        let up = CueQuery::parse(&up_message).unwrap();
        assert!(matches!(up.kind, CueQueryKind::Sender(SenderQuery::Updates)));
        assert_eq!(up.read(&workspace, sender_peer, 0), Some(serde_json::json!(false)));
        crate::engine::osc_updates::set_subscription(sender_peer, true);
        assert_eq!(up.read(&workspace, sender_peer, 0), Some(serde_json::json!(true)));
        crate::engine::osc_updates::set_subscription(sender_peer, false);

        // Scoped forms read the same sender status once the workspace matches.
        let scoped = format!("/workspace/{}/updates", workspace.instance_id());
        let scoped_message = query_message(&scoped);
        let scoped_query = CueQuery::parse(&scoped_message).unwrap();
        assert!(matches!(scoped_query.kind, CueQueryKind::Sender(SenderQuery::Updates)));

        // The write forms (one argument) stay commands, not queries.
        let mut write = query_message("/alwaysReply");
        write.args.push(rosc::OscType::Int(1));
        assert!(CueQuery::parse(&write).is_none());
        assert!(matches!(resolve_action("/alwaysReply", &[rosc::OscType::Int(1)]), OscAction::AlwaysReply(true)));
        let mut write_updates = query_message("/updates");
        write_updates.args.push(rosc::OscType::Int(1));
        assert!(CueQuery::parse(&write_updates).is_none());

        // Sender status reads are any-access, like the app queries; cue queries are not.
        assert!(ar.is_any_access() && up.is_any_access());
        assert!(!CueQuery::parse(&query_message("/cue/1/number")).unwrap().is_any_access());
    }

    #[test]
    fn qlab_client_session_commands_resolve_at_any_access_level() {
        use ClientSessionAction::{Disconnect, Pin};
        assert!(matches!(resolve_action("/disconnect", &[]), OscAction::ClientSession { action: Disconnect }));
        assert!(matches!(resolve_action("/workspace/Blue/disconnect", &[]), OscAction::ClientSession { action: Disconnect }));
        assert!(matches!(resolve_action("/disconnect", &[rosc::OscType::Int(1)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/forgetMeNot", &[rosc::OscType::Int(1)]), OscAction::ClientSession { action: Pin(true) }));
        assert!(matches!(resolve_action("/udpKeepAlive", &[rosc::OscType::String("no".into())]), OscAction::ClientSession { action: Pin(false) }));
        assert!(matches!(resolve_action("/forgetMeNot", &[rosc::OscType::String("maybe".into())]), OscAction::Unmatched));
        // No-arg forgetMeNot is the read form: a query, not a command.
        assert!(matches!(resolve_action("/forgetMeNot", &[]), OscAction::Unmatched));
        let read_message = query_message("/forgetMeNot");
        let read = CueQuery::parse(&read_message).unwrap();
        assert!(matches!(read.kind, CueQueryKind::Sender(SenderQuery::Pinned)));
        assert!(read.is_any_access());
        assert!(CueQuery::parse(&query_message("/udpKeepAlive")).is_some());
        let mut write = query_message("/udpKeepAlive");
        write.args.push(rosc::OscType::Int(1));
        assert!(CueQuery::parse(&write).is_none());
        // Any access level, and on the plain-text port too.
        assert!(matches!(authorized_action("/disconnect", &[], false), OscAction::ClientSession { .. }));
        assert!(matches!(authorized_action("/forgetMeNot", &[rosc::OscType::Int(1)], false),
            OscAction::ClientSession { action: Pin(true) }));
        assert!(parse_text_message(b"/disconnect").is_some());
        assert!(parse_text_message(b"/forgetMeNot 1").is_some());
        assert!(parse_text_message(b"/udpKeepAlive true").is_some());
        assert!(parse_text_message(b"/forgetMeNot").is_none());
        assert!(parse_text_message(b"/disconnect 1").is_none());
    }

    #[test]
    fn qlab_udp_reply_port_resolves_validates_and_reads_at_any_access_level() {
        assert!(matches!(resolve_action("/udpReplyPort", &[rosc::OscType::Int(53001)]), OscAction::SetReplyPort(53001)));
        assert!(matches!(resolve_action("/udpReplyPort", &[rosc::OscType::Double(0.0)]), OscAction::SetReplyPort(0)));
        assert!(matches!(resolve_action("/workspace/Blue/udpReplyPort", &[rosc::OscType::Int(65535)]), OscAction::SetReplyPort(65535)));
        assert!(matches!(resolve_action("/udpReplyPort", &[rosc::OscType::Int(65536)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/udpReplyPort", &[rosc::OscType::Double(1.5)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/udpReplyPort", &[rosc::OscType::String("x".into())]), OscAction::Unmatched));
        // No argument is the read form: a query, not a command.
        assert!(matches!(resolve_action("/udpReplyPort", &[]), OscAction::Unmatched));
        let read_message = query_message("/udpReplyPort");
        let read = CueQuery::parse(&read_message).unwrap();
        assert!(matches!(read.kind, CueQueryKind::Sender(SenderQuery::ReplyPort)));
        assert!(read.is_any_access());
        let mut write = query_message("/udpReplyPort");
        write.args.push(rosc::OscType::Int(1));
        assert!(CueQuery::parse(&write).is_none());
        assert!(matches!(authorized_action("/udpReplyPort", &[rosc::OscType::Int(9)], false), OscAction::SetReplyPort(9)));
        assert!(parse_text_message(b"/udpReplyPort 53001").is_some());
        assert!(parse_text_message(b"/udpReplyPort").is_none());
        assert!(parse_text_message(b"/udpReplyPort 70000").is_none());
    }

    #[test]
    fn qlab_reply_format_parses_renders_tokens_and_tracks_per_sender() {
        let s = |v: &str| rosc::OscType::String(v.to_owned());
        // Write form: exactly one string argument; an empty string resets.
        assert!(matches!(resolve_action("/replyFormat", &[s("#data# #address#")]),
            OscAction::SetReplyFormat(ref f) if f == "#data# #address#"));
        assert!(matches!(resolve_action("/replyFormat", &[s("")]), OscAction::SetReplyFormat(ref f) if f.is_empty()));
        assert!(matches!(resolve_action("/replyFormat", &[]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/replyFormat", &[rosc::OscType::Int(1)]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/replyFormat", &[s("a"), s("b")]), OscAction::Unmatched));
        assert!(matches!(authorized_action("/replyFormat", &[s("x")], false), OscAction::SetReplyFormat(_)));
        assert!(parse_text_message(b"/replyFormat #data#_#address#").is_some());
        assert!(parse_text_message(b"/replyFormat").is_none());
        // Read form: per-sender query, any access.
        let read_message = query_message("/replyFormat");
        let read = CueQuery::parse(&read_message).unwrap();
        assert!(matches!(read.kind, CueQueryKind::Sender(SenderQuery::ReplyFormat)));
        assert!(read.is_any_access());

        // Token rendering: string data bare, other data as JSON, empty on error.
        let id = uuid::Uuid::new_v4();
        assert_eq!(format_reply("#data# #address#", id, "/cue/1/colorName", "ok", &Some(serde_json::json!("green"))),
            "green /cue/1/colorName");
        assert_eq!(format_reply("#status#/#data#", id, "/x", "ok", &Some(serde_json::json!(false))), "ok/false");
        assert_eq!(format_reply("#status#/#data#", id, "/x", "error", &None), "error/");
        assert_eq!(format_reply("#workspace_id#", id, "/x", "ok", &None), id.to_string());

        // Registry: set, read back through the query, empty resets, disconnect clears.
        let fmt_peer: SocketAddr = (Ipv4Addr::LOCALHOST, 16).into();
        assert_eq!(current_reply_format(fmt_peer), None);
        set_reply_format(fmt_peer, Some("#data#".into()));
        assert_eq!(current_reply_format(fmt_peer), Some("#data#".into()));
        assert_eq!(read.read(&Workspace::new("fmt"), fmt_peer, 0), Some(serde_json::json!("#data#")));
        set_reply_format(fmt_peer, Some(String::new()));
        assert_eq!(current_reply_format(fmt_peer), None, "empty format resets to the default");
        set_reply_format(fmt_peer, Some("#status#".into()));
        apply_client_session(ClientSessionAction::Disconnect, fmt_peer);
        assert_eq!(current_reply_format(fmt_peer), None, "disconnect clears the format");

        // Envelope: default JSON without a format, formatted payload with one.
        let message = query_message("/cue/1/colorName");
        let query = CueQuery::parse(&message).unwrap();
        let bytes = query.encode_reply_formatted(id, Some(serde_json::json!("green")), Some("#data# #address#")).unwrap();
        let (_, rosc::OscPacket::Message(reply)) = rosc::decoder::decode_udp(&bytes).unwrap() else { panic!("expected a reply") };
        assert_eq!(reply.addr, "/reply/cue/1/colorName");
        assert_eq!(reply.args, vec![rosc::OscType::String("green /cue/1/colorName".into())]);
        let bytes = query.encode_reply_formatted(id, Some(serde_json::json!("green")), None).unwrap();
        let (_, rosc::OscPacket::Message(reply)) = rosc::decoder::decode_udp(&bytes).unwrap() else { panic!("expected a reply") };
        let [rosc::OscType::String(json)] = reply.args.as_slice() else { panic!("expected JSON data") };
        assert_eq!(serde_json::from_str::<serde_json::Value>(json).unwrap()["data"], "green");
    }

    #[test]
    fn udp_reply_port_registry_tracks_resets_and_expires_unless_pinned() {
        // Port 14/15: other registry tests own 9-13.
        let peer_addr: SocketAddr = (Ipv4Addr::LOCALHOST, 14).into();
        let other_addr: SocketAddr = (Ipv4Addr::LOCALHOST, 15).into();
        assert_eq!(current_reply_port(peer_addr), 14, "default is the sender's own port");
        set_reply_port(peer_addr, Some(53999));
        assert_eq!(current_reply_port(peer_addr), 53999);
        assert_eq!(reply_target(peer_addr), SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 53999));
        set_reply_port(peer_addr, None);
        assert_eq!(current_reply_port(peer_addr), 14, "removal resets to the default");

        // Stale entries expire; pinned ones survive.
        set_reply_port(peer_addr, Some(53999));
        set_reply_port(other_addr, Some(53998));
        crate::engine::osc_updates::pin_client(peer_addr, true);
        let stale = Instant::now().checked_sub(Duration::from_secs(120)).unwrap();
        if let Ok(mut guard) = REPLY_PORTS.lock() {
            let map = guard.as_mut().unwrap();
            map.insert(peer_addr, (53999, stale));
            map.insert(other_addr, (53998, stale));
        }
        assert_eq!(current_reply_port(peer_addr), 53999, "pinned reply ports survive");
        assert_eq!(current_reply_port(other_addr), 15, "stale reply ports expire to the default");
        crate::engine::osc_updates::pin_client(peer_addr, false);
        set_reply_port(peer_addr, None);
        set_reply_port(other_addr, None);
    }

    #[test]
    fn udp_reply_port_redirects_query_replies_to_the_requested_socket() {
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let protocol = Protocol::Osc;
        // Empty allowlist: /version and the /udpReplyPort read are any-access.
        let config = OscReceiveConfig { enabled: true, port: 0, ..OscReceiveConfig::default() };
        let listener = Listener::spawn(
            protocol, protocol.settings(&config), || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            |packet, _allowed, src| {
                // Mirror the production dispatch for per-sender commands.
                if let rosc::OscPacket::Message(message) = packet {
                    match resolve_action(&message.addr, &message.args) {
                        OscAction::SetReplyPort(port) => apply_reply_port(port, src),
                        OscAction::Updates(on) => crate::engine::osc_updates::set_subscription(src, on),
                        OscAction::ClientSession { action } => apply_client_session(action, src),
                        _ => {}
                    }
                }
            },
            |query, src| {
                let data = match &query.kind {
                    CueQueryKind::Sender(SenderQuery::ReplyPort) => Some(serde_json::json!(current_reply_port(src))),
                    _ => Some(serde_json::json!("x")),
                };
                query.encode_reply_formatted(uuid::Uuid::new_v4(), data, None)
            },
        );
        let OscListenerStatus::Listening { address } = statuses.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected an OSC socket");
        };
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let reply_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        for socket in [&sender, &reply_sock] {
            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        }
        let mut response = [0; 1024];
        let send = |addr: &str, args: Vec<rosc::OscType>| {
            let packet = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
                addr: addr.into(), args,
            })).unwrap();
            sender.send_to(&packet, address).unwrap();
        };
        let recv_on = |socket: &UdpSocket, response: &mut [u8; 1024]| -> (String, serde_json::Value) {
            let (length, _origin) = socket.recv_from(response).unwrap();
            let (_, rosc::OscPacket::Message(message)) = rosc::decoder::decode_udp(&response[..length]).unwrap() else {
                panic!("expected a single OSC reply");
            };
            let [rosc::OscType::String(json)] = message.args.as_slice() else { panic!("expected JSON data") };
            (message.addr, serde_json::from_str(json).unwrap())
        };

        // Point replies at the second socket, then query: the reply lands there.
        send("/udpReplyPort", vec![rosc::OscType::Int(reply_sock.local_addr().unwrap().port() as i32)]);
        send("/version", vec![]);
        let (addr, _json) = recv_on(&reply_sock, &mut response);
        assert_eq!(addr, "/reply/version");
        sender.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        assert!(sender.recv_from(&mut response).is_err(), "the sender's own socket gets nothing");
        sender.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        // The no-arg read reports the custom port, delivered to the same socket.
        send("/udpReplyPort", vec![]);
        let (addr, json) = recv_on(&reply_sock, &mut response);
        assert_eq!(addr, "/reply/udpReplyPort");
        assert_eq!(json["data"], reply_sock.local_addr().unwrap().port());

        // `/udpReplyPort 0` restores the default: replies return to the sender.
        send("/udpReplyPort", vec![rosc::OscType::Int(0)]);
        send("/version", vec![]);
        let (addr, _json) = recv_on(&sender, &mut response);
        assert_eq!(addr, "/reply/version");
        listener.stop();
    }

    #[test]
    fn pinned_clients_survive_the_keepalive_window_and_disconnect_clears_everything() {
        // Port 12: ports 9/10/11 are owned by the other registry tests.
        let pinned_peer: SocketAddr = (Ipv4Addr::LOCALHOST, 12).into();
        let other_peer: SocketAddr = (Ipv4Addr::LOCALHOST, 13).into();
        let stale = Instant::now().checked_sub(Duration::from_secs(120)).unwrap();

        set_always_reply(pinned_peer, true);
        set_always_reply(other_peer, true);
        crate::engine::osc_updates::set_subscription(pinned_peer, true);
        crate::engine::osc_updates::set_subscription(other_peer, true);
        crate::engine::osc_updates::pin_client(pinned_peer, true);
        // Age both entries past the 61 s window, in both registries.
        if let Ok(mut guard) = ALWAYS_REPLY.lock() {
            let map = guard.as_mut().unwrap();
            map.insert(pinned_peer, stale);
            map.insert(other_peer, stale);
        }
        crate::engine::osc_updates::age_subscription_for_test(pinned_peer, 120);
        crate::engine::osc_updates::age_subscription_for_test(other_peer, 120);
        // Pinned sender keeps both registrations; the unpinned one is pruned.
        assert!(always_reply_enabled(pinned_peer));
        assert!(!always_reply_enabled(other_peer));
        assert!(crate::engine::osc_updates::is_subscribed(pinned_peer));
        assert!(!crate::engine::osc_updates::is_subscribed(other_peer));

        // /disconnect drops the pin, the alwaysReply entry and the subscription.
        crate::engine::osc_updates::disconnect_client(pinned_peer);
        set_always_reply(pinned_peer, false);
        assert!(!crate::engine::osc_updates::is_pinned(pinned_peer));
        assert!(!always_reply_enabled(pinned_peer));
        assert!(!crate::engine::osc_updates::is_subscribed(pinned_peer));
    }

    #[test]
    fn disconnect_stops_always_reply_and_updates_end_to_end_over_udp() {
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let protocol = Protocol::Osc;
        let config = OscReceiveConfig {
            enabled: true,
            port: 0,
            allowed_ips: vec!["127.0.0.1".into()],
            ..OscReceiveConfig::default()
        };
        let listener = Listener::spawn(
            protocol, protocol.settings(&config), || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            |packet, _allowed, src| {
                // Mirror the production dispatch for client-session commands.
                if let rosc::OscPacket::Message(message) = packet {
                    if let OscAction::ClientSession { action } = resolve_action(&message.addr, &message.args) {
                        apply_client_session(action, src);
                    }
                    if let OscAction::Updates(on) = resolve_action(&message.addr, &message.args) {
                        crate::engine::osc_updates::set_subscription(src, on);
                    }
                }
            },
            |_query, _src| None,
        );
        let OscListenerStatus::Listening { address } = statuses.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected an OSC socket");
        };
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        sender.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let src = sender.local_addr().unwrap();
        let mut response = [0; 1024];
        let send = |addr: &str, args: Vec<rosc::OscType>| {
            let packet = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
                addr: addr.into(), args,
            })).unwrap();
            sender.send_to(&packet, address).unwrap();
        };
        let recv_reply = |response: &mut [u8; 1024]| -> String {
            let (length, _origin) = sender.recv_from(response).unwrap();
            let (_, rosc::OscPacket::Message(message)) = rosc::decoder::decode_udp(&response[..length]).unwrap() else {
                panic!("expected a single OSC reply");
            };
            message.addr
        };

        send("/alwaysReply", vec![rosc::OscType::Int(1)]);
        assert_eq!(recv_reply(&mut response), "/reply/alwaysReply");
        // Once alwaysReply is on, every command is acknowledged in order.
        send("/updates", vec![rosc::OscType::Int(1)]);
        assert_eq!(recv_reply(&mut response), "/reply/updates");
        send("/forgetMeNot", vec![rosc::OscType::Int(1)]);
        assert_eq!(recv_reply(&mut response), "/reply/forgetMeNot");
        // /thump's reply also proves the two packets above were processed first.
        send("/thump", vec![]);
        assert_eq!(recv_reply(&mut response), "/reply/thump");
        assert!(always_reply_enabled(src));
        assert!(crate::engine::osc_updates::is_subscribed(src));
        assert!(crate::engine::osc_updates::is_pinned(src));

        send("/disconnect", vec![]);
        assert_eq!(recv_reply(&mut response), "/reply/disconnect");
        assert!(!always_reply_enabled(src), "disconnect clears alwaysReply");
        assert!(!crate::engine::osc_updates::is_subscribed(src), "disconnect clears updates");
        assert!(!crate::engine::osc_updates::is_pinned(src), "disconnect clears the pin");
        send("/panic", vec![]);
        sender.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        assert!(sender.recv_from(&mut response).is_err(), "no replies after disconnect");
        listener.stop();
    }

    #[test]
    fn override_enabled_read_forms_are_app_queries_and_writes_stay_commands() {
        let workspace = Workspace::new("Override reads");

        let read_message = query_message("/overrides/dmxOutputEnabled");
        let read = CueQuery::parse(&read_message).unwrap();
        assert!(matches!(read.kind, CueQueryKind::App(AppQueryKind::Override("dmx_output"))));
        assert!(read.read(&workspace, peer(), 0).unwrap().is_boolean(),
            "the read returns the current override state");
        assert!(read.is_any_access());

        // Writes and toggles stay commands; the no-arg Enabled form no longer toggles.
        let mut write = query_message("/overrides/dmxOutputEnabled");
        write.args.push(rosc::OscType::Int(1));
        assert!(CueQuery::parse(&write).is_none());
        assert!(matches!(resolve_action("/overrides/dmxOutputEnabled", &[rosc::OscType::Int(1)]),
            OscAction::SetOverride { key: "dmx_output", value: Some(true) }));
        assert!(matches!(resolve_action("/overrides/dmxOutputEnabled", &[]), OscAction::Unmatched));
        assert!(matches!(resolve_action("/overrides/toggleDmxOutput", &[]),
            OscAction::SetOverride { key: "dmx_output", value: None }));
        assert!(CueQuery::parse(&query_message("/overrides/toggleDmxOutput")).is_none());
        assert!(CueQuery::parse(&query_message("/overrides/bogusEnabled")).is_none());
        assert!(CueQuery::parse(&query_message("/workspace/Blue/overrides/dmxOutputEnabled")).is_none());
    }

    #[test]
    fn app_and_sender_status_queries_answer_even_without_an_allowlist() {
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let (seen_tx, seen) = crossbeam_channel::unbounded();
        let protocol = Protocol::Osc;
        let config = OscReceiveConfig { enabled: true, port: 0, ..OscReceiveConfig::default() };
        let listener = Listener::spawn(
            protocol, protocol.settings(&config), || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            |_, _, _| {},
            move |query, _src| {
                let _ = seen_tx.send(query.address.to_string());
                Some(query_packet(&format!("/reply{}", query.address)))
            },
        );
        let OscListenerStatus::Listening { address } = statuses.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected an OSC socket");
        };
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        sender.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut response = [0; 1024];

        // QLab always accepts the app-level and sender-status reads, no passcode needed.
        for query in ["/version", "/workspaces", "/alwaysReply", "/updates", "/overrides/dmxOutputEnabled"] {
            sender.send_to(&query_packet(query), address).unwrap();
            assert_eq!(seen.recv_timeout(Duration::from_secs(2)).unwrap(), query);
            let (length, _origin) = sender.recv_from(&mut response).unwrap();
            let (_, rosc::OscPacket::Message(message)) = rosc::decoder::decode_udp(&response[..length]).unwrap() else {
                panic!("expected a single OSC reply");
            };
            assert_eq!(message.addr, format!("/reply{query}"));
        }
        // Show-data queries still require the allowlist.
        sender.send_to(&query_packet("/cue/101/number"), address).unwrap();
        sender.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        assert!(sender.recv_from(&mut response).is_err());
        assert!(seen.recv_timeout(Duration::from_millis(300)).is_err());
        listener.stop();
    }

    #[test]
    fn app_level_queries_reach_the_query_handler_over_udp() {
        let (status_tx, statuses) = crossbeam_channel::unbounded();
        let (seen_tx, seen) = crossbeam_channel::unbounded();
        let protocol = Protocol::Osc;
        let config = OscReceiveConfig {
            enabled: true,
            port: 0,
            allowed_ips: vec!["127.0.0.1".into()],
            ..OscReceiveConfig::default()
        };
        let listener = Listener::spawn(
            protocol, protocol.settings(&config), || Ipv4Addr::LOCALHOST.into(),
            move |status| { let _ = status_tx.send(status.clone()); },
            |_, _, _| {},
            move |query, _src| {
                let _ = seen_tx.send(query.address.to_string());
                Some(query_packet(&format!("/reply{}", query.address)))
            },
        );
        let OscListenerStatus::Listening { address } = statuses.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected an OSC socket");
        };
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        sender.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut response = [0; 1024];
        for query in ["/version", "/workspaces"] {
            sender.send_to(&query_packet(query), address).unwrap();
            assert_eq!(seen.recv_timeout(Duration::from_secs(2)).unwrap(), query);
            let (length, origin) = sender.recv_from(&mut response).unwrap();
            assert_eq!(origin, address);
            let (_, rosc::OscPacket::Message(message)) = rosc::decoder::decode_udp(&response[..length]).unwrap() else {
                panic!("expected a single OSC reply");
            };
            assert_eq!(message.addr, format!("/reply{query}"));
        }
        listener.stop();
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
        assert_eq!(query.read(&workspace, peer(), 0), Some("1".into()));
        let list = workspace.active_cue_list_mut().unwrap();
        list.get_mut_recursive(&group_id).unwrap().child_cues_mut().unwrap().push(Box::new(MemoCue::new()));
        assert_eq!(query.read(&workspace, peer(), 0), None);
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
            |_, _, _| {},
            move |query, _src| query.encode_reply_formatted(instance, Some(false.into()), None),
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
            move |packet, explicitly_allowed, _src| { let _ = packet_tx.send((packet.clone(), explicitly_allowed)); },
            |_query, _src| None,
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
            |_, _, _| {},
            |_query, _src| None,
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
            move |packet, _, _src| { let _ = packet_tx.send(packet.clone()); },
            |_query, _src| None,
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
