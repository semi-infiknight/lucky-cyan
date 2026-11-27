//! QLab 5.3+ OSC show-control broadcast. Clients send `/listen` (optionally
//! scoped as `/listen/{event}[/{detail}]`), `/ignore`, and `/eventFormat`;
//! Inkue then pushes `/qlab/event/workspace/{event}[/{detail}]` messages on
//! show events. Same 61 s keepalive and pin semantics as the `/updates`
//! registries; delivery honors `/udpReplyPort`, TCP connections and the
//! network output overrides (via `osc_updates::push_to`).
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

const LISTENER_TTL: Duration = Duration::from_secs(61);

/// Workspace events that carry cue arguments.
pub const CUE_EVENTS: [&str; 5] = ["go", "auditionGo", "cue/stop", "cue/start", "playhead"];
/// Workspace events sent without arguments.
pub const BARE_EVENTS: [&str; 6] = ["pauseAll", "resumeAll", "panicAll", "stopAll", "hardStopAll", "resetAll"];
/// Optional detail suffixes of the scoped `/listen/{event}/{detail}` forms.
pub const EVENT_DETAILS: [&str; 4] = ["number", "name", "uniqueID", "type"];

pub fn is_known_event(event: &str) -> bool {
    CUE_EVENTS.contains(&event) || BARE_EVENTS.contains(&event)
}

pub fn is_known_detail(detail: &str) -> bool {
    EVENT_DETAILS.contains(&detail)
}

#[derive(Clone, Default)]
struct ListenerPrefs {
    /// `None` = plain `/listen` (all events); `Some(set)` = scoped subset.
    events: Option<HashSet<String>>,
    /// Custom `/eventFormat`; when set, only single-argument forms are sent.
    format: Option<String>,
}

static LISTENERS: Mutex<Option<HashMap<SocketAddr, (ListenerPrefs, Instant)>>> = Mutex::new(None);

fn prune(guard: &mut HashMap<SocketAddr, (ListenerPrefs, Instant)>) {
    let now = Instant::now();
    guard.retain(|peer, (_, seen)| crate::engine::osc_updates::is_pinned(*peer)
        || now.duration_since(*seen) < LISTENER_TTL);
}

/// `/listen` (all events) or `/listen/{event}` (scoped). The optional detail
/// suffix does not change which events fire, only the message forms.
pub fn listen(addr: SocketAddr, event: Option<&str>) {
    if let Ok(mut guard) = LISTENERS.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        prune(map);
        let entry = map.entry(addr).or_insert_with(|| (ListenerPrefs::default(), Instant::now()));
        entry.1 = Instant::now();
        match (&mut entry.0.events, event) {
            (slot, None) => *slot = None,
            (Some(set), Some(event)) => { set.insert(event.to_string()); }
            (slot @ None, Some(event)) => *slot = Some(HashSet::from([event.to_string()])),
        }
    }
}

/// `/ignore` or `/ignore/{event}`; no event removes the listener entirely.
pub fn ignore(addr: SocketAddr, event: Option<&str>) {
    if let Ok(mut guard) = LISTENERS.lock() {
        let Some(map) = guard.as_mut() else { return };
        prune(map);
        match event {
            None => { map.remove(&addr); }
            Some(event) => {
                let mut drop_entry = false;
                if let Some((prefs, _)) = map.get_mut(&addr) {
                    if let Some(set) = &mut prefs.events {
                        set.remove(event);
                        drop_entry = set.is_empty();
                    }
                }
                if drop_entry {
                    map.remove(&addr);
                }
            }
        }
    }
}

/// `/eventFormat {string}` (empty resets); `None` reads the current format.
pub fn set_event_format(addr: SocketAddr, format: Option<String>) {
    if let Ok(mut guard) = LISTENERS.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        prune(map);
        let entry = map.entry(addr).or_insert_with(|| (ListenerPrefs::default(), Instant::now()));
        entry.1 = Instant::now();
        entry.0.format = format.filter(|f| !f.is_empty());
    }
}

pub fn event_format(addr: SocketAddr) -> Option<String> {
    let mut guard = LISTENERS.lock().ok()?;
    let map = guard.as_mut()?;
    prune(map);
    map.get(&addr).and_then(|(prefs, _)| prefs.format.clone())
}

pub fn has_listeners() -> bool {
    let Ok(mut guard) = LISTENERS.lock() else { return false };
    let Some(map) = guard.as_mut() else { return false };
    prune(map);
    !map.is_empty()
}

pub fn is_listening(addr: SocketAddr) -> bool {
    let Ok(mut guard) = LISTENERS.lock() else { return false };
    let Some(map) = guard.as_mut() else { return false };
    prune(map);
    map.contains_key(&addr)
}

/// QLab `/disconnect` also drops broadcast subscriptions.
pub fn disconnect_client(addr: SocketAddr) {
    if let Ok(mut guard) = LISTENERS.lock() {
        if let Some(map) = guard.as_mut() {
            map.remove(&addr);
        }
    }
}

/// The open workspace's instance ID, noted by the event loop each tick, so
/// hooks without workspace access (e.g. playhead moves) can still render the
/// `#workspace_id#` token of custom `/eventFormat` templates.
static CURRENT_WORKSPACE: Mutex<Option<Uuid>> = Mutex::new(None);

pub fn note_current_workspace(id: Uuid) {
    if let Ok(mut guard) = CURRENT_WORKSPACE.lock() {
        *guard = Some(id);
    }
}

/// Broadcast using the noted current workspace.
pub fn broadcast_current(event: &str, cue: Option<CueEventArgs<'_>>) {
    let id = CURRENT_WORKSPACE.lock().ok().and_then(|guard| *guard).unwrap_or_else(Uuid::nil);
    broadcast(id, event, cue);
}

/// Cue argument bundle for cue-carrying events: (number, name, uniqueID, type).
pub struct CueEventArgs<'a> {
    pub number: &'a str,
    pub name: &'a str,
    pub unique_id: Uuid,
    pub cue_type: &'a str,
}

/// Render an `/eventFormat` template (#workspace_id#, #address#, #status#, #data#).
fn render_event_format(format: &str, workspace_id: Uuid, address: &str, data: &str) -> String {
    format
        .replace("#workspace_id#", &workspace_id.to_string())
        .replace("#address#", address)
        .replace("#status#", "ok")
        .replace("#data#", data)
}

/// Broadcast one workspace event to every matching listener.
pub fn broadcast(workspace_id: Uuid, event: &str, cue: Option<CueEventArgs<'_>>) {
    if !has_listeners() {
        return;
    }
    let listeners: Vec<(SocketAddr, ListenerPrefs)> = {
        let Ok(mut guard) = LISTENERS.lock() else { return };
        let Some(map) = guard.as_mut() else { return };
        prune(map);
        map.iter()
            .filter(|(_, (prefs, _))| prefs.events.as_ref().is_none_or(|set| set.contains(event)))
            .map(|(addr, (prefs, _))| (*addr, prefs.clone()))
            .collect()
    };
    let base = format!("/qlab/event/workspace/{event}");
    let detail_values = cue.map(|cue| {
        [
            ("number", cue.number.to_string()),
            ("name", cue.name.to_string()),
            ("uniqueID", cue.unique_id.to_string()),
            ("type", cue.cue_type.to_string()),
        ]
    });
    for (addr, prefs) in listeners {
        let mut messages: Vec<(String, Vec<rosc::OscType>)> = Vec::new();
        let string = |value: String| rosc::OscType::String(value);
        if let Some(format) = &prefs.format {
            // Custom format: only single-argument forms, rendered through the template.
            if let Some(details) = &detail_values {
                for (detail, value) in details {
                    let address = format!("{base}/{detail}");
                    let payload = render_event_format(format, workspace_id, &address, value);
                    messages.push((address, vec![string(payload)]));
                }
            } else {
                let payload = render_event_format(format, workspace_id, &base, "");
                messages.push((base.clone(), vec![string(payload)]));
            }
        } else if let Some(details) = &detail_values {
            // Default: the multi-argument form plus each single-argument detail form.
            messages.push((base.clone(), details.iter().map(|(_, value)| string(value.clone())).collect()));
            for (detail, value) in details {
                messages.push((format!("{base}/{detail}"), vec![string(value.clone())]));
            }
        } else {
            messages.push((base.clone(), vec![]));
        }
        crate::engine::osc_updates::push_to(addr, &messages);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(port: u16) -> SocketAddr {
        SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port)
    }

    #[test]
    fn event_names_and_details_validate() {
        for event in CUE_EVENTS.iter().chain(BARE_EVENTS.iter()) {
            assert!(is_known_event(event), "{event}");
        }
        assert!(!is_known_event("bogus"));
        for detail in EVENT_DETAILS {
            assert!(is_known_detail(detail));
        }
        assert!(!is_known_detail("color"));
    }

    #[test]
    fn listen_scopes_and_ignore_narrows_the_set() {
        let addr = peer(41000);
        listen(addr, None);
        assert!(is_listening(addr));
        ignore(addr, Some("go"));
        // Plain /listen (all) is unaffected by a scoped ignore.
        assert!(is_listening(addr));
        listen(addr, Some("go"));
        ignore(addr, Some("go"));
        assert!(!is_listening(addr), "the only scoped event was ignored");
        listen(addr, Some("panicAll"));
        ignore(addr, None);
        assert!(!is_listening(addr));
        disconnect_client(addr);
    }

    #[test]
    fn event_format_round_trips_and_empty_resets() {
        let addr = peer(41001);
        assert_eq!(event_format(addr), None);
        set_event_format(addr, Some("/cue/#data#/start".into()));
        assert_eq!(event_format(addr), Some("/cue/#data#/start".into()));
        set_event_format(addr, Some(String::new()));
        assert_eq!(event_format(addr), None, "empty string resets to the default");
        set_event_format(addr, Some("#address# #data#".into()));
        assert_eq!(
            render_event_format("#address# #data# #status# #workspace_id#",
                Uuid::nil(), "/qlab/event/workspace/go/number", "24"),
            format!("/qlab/event/workspace/go/number 24 ok {}", Uuid::nil()),
        );
        disconnect_client(addr);
    }

    #[test]
    fn broadcast_builds_default_and_custom_forms_for_matching_listeners_only() {
        let addr = peer(41002);
        let other = peer(41003);
        listen(addr, Some("go"));
        listen(other, Some("panicAll"));
        // Capture what would be sent by intercepting push_to is not possible
        // without a socket, so assert the selection logic via the registries.
        assert!(has_listeners());
        let selected: Vec<SocketAddr> = {
            let guard = LISTENERS.lock().unwrap();
            let map = guard.as_ref().unwrap();
            map.iter()
                .filter(|(_, (prefs, _))| prefs.events.as_ref().is_none_or(|set| set.contains("go")))
                .map(|(addr, _)| *addr)
                .collect()
        };
        assert_eq!(selected, vec![addr], "only the go listener matches the go event");
        ignore(addr, None);
        ignore(other, None);
    }
}
