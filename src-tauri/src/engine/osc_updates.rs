//! QLab `/updates` push channel. A client that sends `/updates 1` receives
//! `/update/workspace/{id}...` messages whenever show state changes, until it
//! sends `/updates 0`. Like QLab, UDP subscribers expire after 61 s without a
//! refresh (any message from them refreshes; `/updates 1` does).
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// QLab's UDP client keepalive window.
const SUBSCRIBER_TTL: Duration = Duration::from_secs(61);

static SUBSCRIBERS: Mutex<Option<HashMap<SocketAddr, Instant>>> = Mutex::new(None);

/// QLab `/forgetMeNot` / `/udpKeepAlive 1`: pinned clients never expire.
static PINNED: Mutex<Option<HashSet<SocketAddr>>> = Mutex::new(None);

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
    map.retain(|peer, seen| is_pinned(*peer) || now.duration_since(*seen) < SUBSCRIBER_TTL);
    map.contains_key(&addr)
}

/// QLab `/forgetMeNot {bool}` / `/udpKeepAlive {bool}`: pin the sender beyond
/// the 61 s keepalive (or release the pin with `false`).
pub fn pin_client(addr: SocketAddr, on: bool) {
    let Ok(mut guard) = PINNED.lock() else { return };
    let set = guard.get_or_insert_with(HashSet::new);
    if on { set.insert(addr); } else { set.remove(&addr); }
}

/// Is this client pinned? Pinned clients are never pruned from any registry.
pub fn is_pinned(addr: SocketAddr) -> bool {
    let Ok(guard) = PINNED.lock() else { return false };
    guard.as_ref().is_some_and(|set| set.contains(&addr))
}

/// QLab `/disconnect`: drop every trace of the client (updates subscription
/// and pin; the OSC server clears its alwaysReply entry too).
pub fn disconnect_client(addr: SocketAddr) {
    set_subscription(addr, false);
    if let Ok(mut guard) = PINNED.lock() {
        if let Some(set) = guard.as_mut() {
            set.remove(&addr);
        }
    }
}

/// Test-only: age a subscriber entry past the keepalive window.
#[cfg(test)]
pub(crate) fn age_subscription_for_test(addr: SocketAddr, seconds: u64) {
    if let Ok(mut guard) = SUBSCRIBERS.lock() {
        if let Some(map) = guard.as_mut() {
            map.insert(addr, Instant::now() - Duration::from_secs(seconds));
        }
    }
}

/// Any live subscriber? Also prunes expired ones.
pub fn has_subscribers() -> bool {
    let Ok(mut guard) = SUBSCRIBERS.lock() else { return false };
    let Some(map) = guard.as_mut() else { return false };
    let now = Instant::now();
    map.retain(|peer, seen| is_pinned(*peer) || now.duration_since(*seen) < SUBSCRIBER_TTL);
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
    map.retain(|peer, seen| is_pinned(*peer) || now.duration_since(*seen) < SUBSCRIBER_TTL);
    if map.is_empty() {
        return;
    }
    let Ok(socket) = super::net_interface::udp_send_socket() else { return };
    for (addr, args) in messages {
        let packet = rosc::OscPacket::Message(rosc::OscMessage { addr: addr.clone(), args: args.clone() });
        let Ok(bytes) = rosc::encoder::encode(&packet) else { continue };
        let overrides = &super::overrides::OVERRIDES;
        for peer in map.keys() {
            // Honor QLab `/udpReplyPort` for this subscriber.
            let target = super::osc_server::reply_target(*peer);
            // QLab network output overrides apply to pushes too.
            let allowed = if target.ip().is_loopback() {
                overrides.network_local_output()
            } else {
                overrides.network_external_output()
            };
            if !allowed {
                continue;
            }
            let _ = socket.send_to(&bytes, target);
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
        // Per-sender assertions: has_subscribers() is global and other tests
        // may hold live subscriptions in parallel.
        let addr: SocketAddr = "127.0.0.1:53001".parse().unwrap();
        set_subscription(addr, true);
        assert!(is_subscribed(addr));
        set_subscription(addr, false);
        assert!(!is_subscribed(addr));
        set_subscription(addr, true);
        // Age the entry past the TTL.
        if let Ok(mut guard) = SUBSCRIBERS.lock() {
            if let Some(map) = guard.as_mut() {
                map.insert(addr, Instant::now() - Duration::from_secs(120));
            }
        }
        assert!(!is_subscribed(addr), "stale subscribers are pruned");
        set_subscription(addr, false);
    }

    #[test]
    fn pinned_subscriptions_survive_the_ttl_and_disconnect_clears_them() {
        let pinned: SocketAddr = "127.0.0.1:53011".parse().unwrap();
        let plain: SocketAddr = "127.0.0.1:53012".parse().unwrap();
        set_subscription(pinned, true);
        set_subscription(plain, true);
        pin_client(pinned, true);
        let stale = Instant::now() - Duration::from_secs(120);
        if let Ok(mut guard) = SUBSCRIBERS.lock() {
            if let Some(map) = guard.as_mut() {
                map.insert(pinned, stale);
                map.insert(plain, stale);
            }
        }
        assert!(is_subscribed(pinned), "pinned clients are never pruned");
        assert!(!is_subscribed(plain), "unpinned stale subscribers are pruned");
        disconnect_client(pinned);
        assert!(!is_pinned(pinned));
        assert!(!is_subscribed(pinned));
        set_subscription(plain, false);
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
