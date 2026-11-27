//! OSC argument and message types used by [`super::osc_cue::OscCue`].

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single OSC argument value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum OscArg {
    Int(i32),
    Float(f32),
    Str(String),
    Bool(bool),
}

/// Unframed data for stage equipment using a raw UDP protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "format", content = "value", rename_all = "snake_case")]
pub enum RawDatagram {
    Text(String),
    Hex(String),
}

/// One OSC message to send on GO — targets a named patch with an address and args.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OscMessage {
    /// ID of the [`crate::engine::osc_patch::OscPatch`] to send to.
    pub patch_id: Uuid,
    /// OSC address string, e.g. `"/my/device/volume"`.
    pub address: String,
    /// Arguments to include in the message.
    pub args: Vec<OscArg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawDatagram>,
}

impl OscMessage {
    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        let bytes = match &self.raw {
            Some(RawDatagram::Text(text)) => text.as_bytes().to_vec(),
            Some(RawDatagram::Hex(text)) => {
                let digits: Vec<_> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
                anyhow::ensure!(
                    digits.len() % 2 == 0 && digits.iter().all(u8::is_ascii_hexdigit),
                    "Use two hex digits per byte, optionally separated by spaces"
                );
                digits
                    .chunks_exact(2)
                    .map(|pair| {
                        let hex = std::str::from_utf8(pair)?;
                        Ok(u8::from_str_radix(hex, 16)?)
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?
            }
            None => {
                let args = self
                    .args
                    .iter()
                    .map(|arg| match arg {
                        OscArg::Int(i) => rosc::OscType::Int(*i),
                        OscArg::Float(f) => rosc::OscType::Float(*f),
                        OscArg::Str(s) => rosc::OscType::String(s.clone()),
                        OscArg::Bool(b) => rosc::OscType::Bool(*b),
                    })
                    .collect();
                rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
                    addr: self.address.clone(),
                    args,
                }))?
            }
        };
        anyhow::ensure!(
            !bytes.is_empty() && bytes.len() <= 65_507,
            "UDP payload must contain 1 to 65507 bytes"
        );
        Ok(bytes)
    }

    pub fn send(&self, target: &str) -> anyhow::Result<usize> {
        let overrides = &crate::engine::overrides::OVERRIDES;
        anyhow::ensure!(
            network_output_allowed(target, overrides.network_external_output(), overrides.network_local_output()),
            "network output to '{target}' is disabled by the override window"
        );
        let bytes = self.encode()?;
        Ok(crate::engine::net_interface::udp_send_socket()?.send_to(&bytes, target)?)
    }
}

/// QLab network output overrides: external targets need `network_external_output`,
/// loopback targets need `network_local_output`. Pure so tests need no sockets.
pub fn network_output_allowed(target: &str, external_output: bool, local_output: bool) -> bool {
    let host = target.rsplit_once(':').map(|(host, _)| host).unwrap_or(target);
    let local = host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if local { local_output } else { external_output }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_output_gate_splits_external_and_loopback_targets() {
        // Both overrides on: everything allowed.
        assert!(network_output_allowed("192.168.1.20:53000", true, true));
        assert!(network_output_allowed("127.0.0.1:53001", true, true));
        assert!(network_output_allowed("localhost:53001", true, true));
        // External off: only loopback goes out.
        assert!(!network_output_allowed("192.168.1.20:53000", false, true));
        assert!(network_output_allowed("127.0.0.1:53001", false, true));
        assert!(network_output_allowed("localhost:9", false, true));
        // Local off: only external goes out.
        assert!(network_output_allowed("192.168.1.20:53000", true, false));
        assert!(!network_output_allowed("127.0.0.1:53001", true, false));
        assert!(!network_output_allowed("LOCALHOST:9", true, false));
        // Bare host without port still classifies.
        assert!(!network_output_allowed("10.0.0.8", false, true));
        assert!(network_output_allowed("10.0.0.8", true, false));
    }

    #[test]
    fn osc_arg_int_roundtrip() {
        let arg = OscArg::Int(42);
        let json = serde_json::to_string(&arg).unwrap();
        let back: OscArg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, OscArg::Int(42));
    }

    #[test]
    fn osc_arg_float_roundtrip() {
        let arg = OscArg::Float(2.5);
        let json = serde_json::to_string(&arg).unwrap();
        let back: OscArg = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, OscArg::Float(f) if (f - 2.5).abs() < 1e-5));
    }

    #[test]
    fn osc_arg_str_roundtrip() {
        let arg = OscArg::Str("hello".to_string());
        let json = serde_json::to_string(&arg).unwrap();
        let back: OscArg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, OscArg::Str("hello".to_string()));
    }

    #[test]
    fn osc_arg_bool_roundtrip() {
        let arg = OscArg::Bool(true);
        let json = serde_json::to_string(&arg).unwrap();
        let back: OscArg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, OscArg::Bool(true));
    }

    #[test]
    fn osc_message_roundtrip() {
        let msg = OscMessage {
            patch_id: Uuid::nil(),
            raw: None,
            address: "/test/addr".to_string(),
            args: vec![
                OscArg::Int(1),
                OscArg::Float(2.0),
                OscArg::Str("x".into()),
                OscArg::Bool(false),
            ],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: OscMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.address, "/test/addr");
        assert_eq!(back.args.len(), 4);
    }
    #[test]
    fn udp_text_and_hex_reach_the_receiver_without_osc_framing() {
        let listener = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut msg: OscMessage = serde_json::from_value(serde_json::json!({
            "patch_id": Uuid::nil(), "address": "/", "args": []
        }))
        .unwrap();
        for raw in [
            RawDatagram::Text("LX1\r".into()),
            RawDatagram::Hex("4c 58 31 0D".into()),
        ] {
            msg.raw = Some(raw);
            msg.send(&listener.local_addr().unwrap().to_string())
                .unwrap();
            let mut buf = [0; 16];
            let n = listener.recv(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"LX1\r");
        }
        msg.raw = Some(RawDatagram::Hex("fZ".into()));
        assert!(msg.encode().is_err());
    }
}
