//! QLab 5 MSC show-control broadcast: when the workspace's MSC Broadcast
//! setting is enabled, GO sends `MSC GO {cue number}` and Stop All /
//! HardStop All / Panic All send `MSC ALL_OFF` while Reset All sends
//! `MSC RESET`, to every configured destination (MIDI output port + command
//! format + device ID). Gated by the workspace setting and the global
//! `mscOutput` override; delivery uses the same MIDI output path as MIDI cues.
use crate::cue::midi_cue::{MidiMessage, MidiMessageType};
use crate::cue::msc::{MscBroadcastDestination, MscCommand, MscMessage};
use crate::preferences::MidiPreferences;

/// Build one wire message per configured destination. Pure so tests can
/// inspect the mapping without opening MIDI ports.
pub(crate) fn messages(midi: &MidiPreferences, command: MscCommand, q_number: &str) -> Vec<MidiMessage> {
    if !midi.msc_broadcast_enabled {
        return Vec::new();
    }
    midi.msc_broadcast_destinations
        .iter()
        .map(|dest: &MscBroadcastDestination| MidiMessage {
            port_name: dest.port_name.clone(),
            message_type: MidiMessageType::Msc,
            channel: 1,
            data1: 0,
            data2: 0,
            msc: Some(MscMessage {
                device_id: dest.device_id,
                command_format: dest.command_format,
                command,
                q_number: q_number.to_string(),
                q_list: String::new(),
                q_path: String::new(),
            }),
            sysex: None,
            fade: None,
        })
        .collect()
}

/// Send one broadcast event to every destination. The global `mscOutput`
/// override is enforced inside `send_midi_messages` (MSC message type);
/// individual port failures are logged and raise health alerts there, so a
/// missing device never fails the show.
pub fn broadcast(midi: &MidiPreferences, command: MscCommand, q_number: &str) {
    let messages = messages(midi, command, q_number);
    if messages.is_empty() {
        return;
    }
    if let Err(error) = crate::cue::midi_cue::send_midi_messages(&messages) {
        log::warn!("MSC broadcast: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(enabled: bool, destinations: Vec<MscBroadcastDestination>) -> MidiPreferences {
        MidiPreferences { msc_broadcast_enabled: enabled, msc_broadcast_destinations: destinations }
    }

    fn dest(device_id: u8) -> MscBroadcastDestination {
        MscBroadcastDestination { port_name: "IAC Bus 1".into(), command_format: 48, device_id }
    }

    #[test]
    fn disabled_or_empty_destinations_produce_no_messages() {
        assert!(messages(&prefs(false, vec![dest(9)]), MscCommand::Go, "24").is_empty());
        assert!(messages(&prefs(true, vec![]), MscCommand::Go, "24").is_empty());
    }

    #[test]
    fn go_carries_the_cue_number_and_bare_commands_carry_none() {
        let prefs = prefs(true, vec![dest(9), dest(127)]);
        let built = messages(&prefs, MscCommand::Go, "24");
        assert_eq!(built.len(), 2);
        for message in &built {
            let msc = message.msc.as_ref().unwrap();
            assert_eq!(msc.command, MscCommand::Go);
            assert_eq!(msc.q_number, "24");
            assert_eq!(message.port_name, "IAC Bus 1");
            assert_eq!(message.message_type, MidiMessageType::Msc);
            assert!(msc.to_broadcast_bytes().is_ok());
        }
        assert_eq!(built[1].msc.as_ref().unwrap().device_id, 127);
        let all_off = messages(&prefs, MscCommand::AllOff, "");
        assert!(all_off.iter().all(|m| {
            let msc = m.msc.as_ref().unwrap();
            msc.command == MscCommand::AllOff && msc.q_number.is_empty()
                && msc.to_broadcast_bytes().unwrap() == [0xf0, 0x7f, msc.device_id, 2, 48, 0x08, 0xf7]
        }));
        let reset = messages(&prefs, MscCommand::Reset, "");
        assert!(reset.iter().all(|m| m.msc.as_ref().unwrap().command == MscCommand::Reset));
    }

    #[test]
    fn invalid_destinations_fail_encoding_not_the_build() {
        let prefs = prefs(true, vec![MscBroadcastDestination {
            port_name: "IAC Bus 1".into(),
            command_format: 0,
            device_id: 200,
        }]);
        let built = messages(&prefs, MscCommand::Go, "24");
        assert_eq!(built.len(), 1);
        assert!(built[0].msc.as_ref().unwrap().to_broadcast_bytes().is_err());
    }
}
