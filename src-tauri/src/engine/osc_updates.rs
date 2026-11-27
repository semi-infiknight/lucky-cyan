//! QLab `/updates` push channel. A client that sends `/updates 1` receives
//! `/update/workspace/{id}...` messages whenever show state changes, until it
//! sends `/updates 0`. Like QLab, UDP subscribers expire after 61 s without a
//! refresh (any message from them refreshes; `/updates 1` does).
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// QLab's UDP client keepalive window.
const SUBSCRIBER_TTL: Duration = Duration::from_secs(61);

static SUBSCRIBERS: Mutex<Option<HashMap<SocketAddr, Instant>>> = Mutex::new(None);

pub fn set_subscription(addr: SocketAddr, on: bool) {
    let Ok(mut guard) = SUBSCRIBERS.lock() else { return };
    let map = guard.get_or_insert_with(HashMap::new);
    if on {
        map.insert(addr, Instant::now());
    } else {
        map.remove(&addr);
    }
}

/// Is this sender currently subscribed? Also prunes expired entries.
pub fn is_subscribed(addr: SocketAddr) -> bool {
    let Ok(mut guard) = SUBSCRIBERS.lock() else { return false };
    let Some(map) = guard.as_mut() else { return false };
    let now = Instant::now();
    map.retain(|_, seen| now.duration_since(*seen) < SUBSCRIBER_TTL);
    map.contains_key(&addr)
}

/// Any live subscriber? Also prunes expired ones.
pub fn has_subscribers() -> bool {
    let Ok(mut guard) = SUBSCRIBERS.lock() else { return false };
    let Some(map) = guard.as_mut() else { return false };
    let now = Instant::now();
    map.retain(|_, seen| now.duration_since(*seen) < SUBSCRIBER_TTL);
    !map.is_empty()
}

/// Refresh a subscriber's keepalive (any incoming message counts).
pub fn touch(addr: SocketAddr) {
    if let Ok(mut guard) = SUBSCRIBERS.lock() {
        if let Some(map) = guard.as_mut() {
            if let Some(seen) = map.get_mut(&addr) {
                *seen = Instant::now();
            }
        }
    }
}

/// Send `/update/...` messages to every live subscriber. `messages` pairs an
/// OSC address with its arguments.
pub fn push(messages: &[(String, Vec<rosc::OscType>)]) {
    if messages.is_empty() {
        return;
    }
    let Ok(mut guard) = SUBSCRIBERS.lock() else { return };
    let Some(map) = guard.as_mut() else { return };
    let now = Instant::now();
    map.retain(|_, seen| now.duration_since(*seen) < SUBSCRIBER_TTL);
    if map.is_empty() {
        return;
    }
    let Ok(socket) = super::net_interface::udp_send_socket() else { return };
    for (addr, args) in messages {
        let packet = rosc::OscPacket::Message(rosc::OscMessage { addr: addr.clone(), args: args.clone() });
        let Ok(bytes) = rosc::encoder::encode(&packet) else { continue };
        for peer in map.keys() {
            let _ = socket.send_to(&bytes, peer);
        }
    }
}

/// Convenience: the workspace-level reload hint.
pub fn update_workspace(workspace_id: Uuid) -> (String, Vec<rosc::OscType>) {
    (format!("/update/workspace/{workspace_id}"), Vec::new())
}

/// Convenience: a cue-list playhead change (`cue_id` argument omitted when None).
pub fn update_playback_position(workspace_id: Uuid, list_id: Uuid, cue_id: Option<Uuid>) -> (String, Vec<rosc::OscType>) {
    let args = cue_id.map(|id| vec![rosc::OscType::String(id.to_string())]).unwrap_or_default();
    (format!("/update/workspace/{workspace_id}/cueList/{list_id}/playbackPosition"), args)
}

/// Convenience: one cue's state changed.
pub fn update_cue(workspace_id: Uuid, cue_id: Uuid) -> (String, Vec<rosc::OscType>) {
    (format!("/update/workspace/{workspace_id}/cue_id/{cue_id}"), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriptions_expire_without_a_refresh() {
        let addr: SocketAddr = "127.0.0.1:53001".parse().unwrap();
        set_subscription(addr, true);
        assert!(has_subscribers());
        set_subscription(addr, false);
        assert!(!has_subscribers());
        set_subscription(addr, true);
        // Age the entry past the TTL.
        if let Ok(mut guard) = SUBSCRIBERS.lock() {
            if let Some(map) = guard.as_mut() {
                map.insert(addr, Instant::now() - Duration::from_secs(120));
            }
        }
        assert!(!has_subscribers(), "stale subscribers are pruned");
        set_subscription(addr, false);
    }

    #[test]
    fn update_addresses_match_the_qlab_shapes() {
        let ws = Uuid::new_v4();
        let list = Uuid::new_v4();
        let cue = Uuid::new_v4();
        assert_eq!(update_workspace(ws).0, format!("/update/workspace/{ws}"));
        let (addr, args) = update_playback_position(ws, list, Some(cue));
        assert_eq!(addr, format!("/update/workspace/{ws}/cueList/{list}/playbackPosition"));
        assert_eq!(args, vec![rosc::OscType::String(cue.to_string())]);
        let (_addr, args) = update_playback_position(ws, list, None);
        assert!(args.is_empty());
        assert_eq!(update_cue(ws, cue).0, format!("/update/workspace/{ws}/cue_id/{cue}"));
    }
}
