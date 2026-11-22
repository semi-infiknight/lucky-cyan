//! Addressed MIDI Show Control 1.1 transport commands.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum MscCommand {
    Go = 0x01,
    Stop = 0x02,
    Resume = 0x03,
    Load = 0x05,
    AllOff = 0x08,
    Restore = 0x09,
    Reset = 0x0a,
    GoOff = 0x0b,
    StandbyNext = 0x11,
    StandbyPrevious = 0x12,
    SequenceNext = 0x13,
    SequencePrevious = 0x14,
}

impl MscCommand {
    pub(crate) fn from_code(code: u64) -> Option<Self> {
        use MscCommand::*;
        [Go, Stop, Resume, Load, AllOff, Restore, Reset, GoOff,
            StandbyNext, StandbyPrevious, SequenceNext, SequencePrevious]
            .into_iter().find(|command| u64::from(*command as u8) == code)
    }
}

/// Incoming transport operations in the active list, or across the desk for
/// an unnumbered Resume and ALL_OFF.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MscTransport {
    Go(Option<String>),
    Pause(String),
    Resume(Option<String>),
    Load(String),
    AllOff,
    Standby(bool),
}

impl MscTransport {
    pub(crate) fn decode(bytes: &[u8], destination: u8) -> Option<Self> {
        if bytes.len() > 128 {
            return None;
        }
        let [0xf0, 0x7f, device, 0x02, 0x01 | 0x10 | 0x30, command, payload @ .., 0xf7] = bytes else {
            return None;
        };
        if destination > 126 || !matches!(*device, 127) && *device != destination {
            return None;
        }
        let command = MscCommand::from_code(u64::from(*command))?;
        if payload.is_empty() {
            return match command {
                MscCommand::Go => Some(Self::Go(None)),
                MscCommand::Resume => Some(Self::Resume(None)),
                MscCommand::AllOff => Some(Self::AllOff),
                MscCommand::StandbyNext => Some(Self::Standby(true)),
                MscCommand::StandbyPrevious => Some(Self::Standby(false)),
                _ => None,
            };
        }
        // Trailing separators can stand for omitted list and path operands.
        // A frame containing a real list or path never resolves in the desk.
        let mut fields = payload.split(|&byte| byte == 0);
        let number = std::str::from_utf8(fields.next()?).ok()?;
        let mut omitted = 0;
        for field in fields {
            omitted += 1;
            if !field.is_empty() || omitted > 2 {
                return None;
            }
        }
        if !valid_cue_number(number) {
            return None;
        }
        match command {
            MscCommand::Go => Some(Self::Go(Some(number.into()))),
            MscCommand::Stop => Some(Self::Pause(number.into())),
            MscCommand::Resume => Some(Self::Resume(Some(number.into()))),
            MscCommand::Load => Some(Self::Load(number.into())),
            _ => None,
        }
    }
}

fn valid_cue_number(number: &str) -> bool {
    number.split('.').all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MscMessage {
    pub device_id: u8,
    pub command_format: u8,
    pub command: MscCommand,
    #[serde(default)]
    pub q_number: String,
    #[serde(default)]
    pub q_list: String,
    #[serde(default)]
    pub q_path: String,
}

impl MscMessage {
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        if self.device_id > 0x6f {
            return Err("MSC needs an individual device ID from 0 to 111.".into());
        }
        // Numeric command formats defined in the QLab parameter reference.
        if !matches!(self.command_format, 1..=6 | 16..=24 | 32..=42 | 48..=56 | 64..=69 | 80..=88 | 96..=100 | 127)
        {
            return Err("Unrecognized MSC command format.".into());
        }
        let cue_fields = [self.q_number.as_str(), &self.q_list, &self.q_path];
        let list_field = [self.q_list.as_str()];
        let fields: &[&str] = match self.command {
            MscCommand::AllOff | MscCommand::Restore | MscCommand::Reset => {
                if cue_fields.iter().any(|field| !field.is_empty()) {
                    return Err("This MSC command has no cue, list or path operands.".into());
                }
                &[]
            }
            MscCommand::StandbyNext | MscCommand::StandbyPrevious |
            MscCommand::SequenceNext | MscCommand::SequencePrevious => {
                if !self.q_number.is_empty() || !self.q_path.is_empty() {
                    return Err("This MSC command accepts only an optional Q list.".into());
                }
                &list_field
            }
            MscCommand::Load if self.q_number.is_empty() => {
                return Err("MSC LOAD requires a Q number.".into());
            }
            _ => &cue_fields,
        };
        let mut bytes = vec![
            0xf0,
            0x7f,
            self.device_id,
            0x02,
            self.command_format,
            self.command as u8,
        ];
        let mut empty_field = false;
        for (index, field) in fields.iter().enumerate() {
            if field.is_empty() {
                empty_field = true;
                continue;
            }
            if empty_field {
                return Err("MSC requires a cue before a list and a list before a path.".into());
            }
            if bytes.len() + usize::from(index > 0) + field.len() + 1 > 128 {
                return Err("MSC messages can contain at most 128 bytes including framing.".into());
            }
            if !valid_cue_number(field) {
                return Err(
                    "MSC cue, list and path use numeric sections separated by periods.".into(),
                );
            }
            if index > 0 {
                bytes.push(0);
            }
            bytes.extend_from_slice(field.as_bytes());
        }
        bytes.push(0xf7);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> MscMessage {
        MscMessage {
            device_id: 17,
            command_format: 16,
            command: MscCommand::Go,
            q_number: "235.6".into(),
            q_list: "36.6".into(),
            q_path: "59".into(),
        }
    }

    #[test]
    fn msc_input_checks_device_format_and_keeps_literal_cue_numbers() {
        let frame = |device, format, command, fields: &[u8]| {
            let mut bytes = vec![0xf0, 0x7f, device, 0x02, format, command];
            bytes.extend_from_slice(fields);
            bytes.push(0xf7);
            bytes
        };
        assert_eq!(MscTransport::decode(&frame(7, 16, 1, b"01.2"), 7),
            Some(MscTransport::Go(Some("01.2".into()))));
        assert_eq!(MscTransport::decode(&frame(127, 48, 2, b"01.2\0"), 7),
            Some(MscTransport::Pause("01.2".into())));
        assert_eq!(MscTransport::decode(&frame(7, 1, 3, b""), 7), Some(MscTransport::Resume(None)));
        assert_eq!(MscTransport::decode(&frame(7, 16, 8, b""), 7), Some(MscTransport::AllOff));
        for rejected in [
            frame(8, 16, 1, b"1"),
            frame(7, 17, 1, b"1"),
            frame(7, 16, 1, b"1\x004"),
            frame(7, 16, 1, b"1\x00\x004"),
            frame(7, 16, 1, b"1..2"),
            frame(7, 16, 2, b""),
            frame(7, 16, 8, b"1"),
            frame(7, 16, 1, &[b'1'; 122]),
        ] {
            assert_eq!(MscTransport::decode(&rejected, 7), None);
        }
    }

    #[test]
    fn additional_msc_commands_use_their_specified_operand_layouts() {
        let mut msg = example();
        for (command, code) in [(MscCommand::Load, 5), (MscCommand::GoOff, 11)] {
            msg.command = command;
            let encoded = msg.to_bytes().unwrap();
            assert_eq!(encoded[5], code);
            assert_eq!(&encoded[6..encoded.len() - 1], b"235.6\x0036.6\x0059");
        }

        msg.q_number.clear();
        msg.q_path.clear();
        for code in 17..=20 {
            msg.command = MscCommand::from_code(code.into()).unwrap();
            assert_eq!(msg.to_bytes().unwrap(), [0xf0, 0x7f, 17, 2, 16, code, b'3', b'6', b'.', b'6', 0xf7]);
        }

        msg.command = MscCommand::Load;
        assert!(msg.to_bytes().is_err());
        for code in 8..=10 {
            msg.command = MscCommand::from_code(code.into()).unwrap();
            assert!(msg.to_bytes().is_err(), "device operations reject leftover operands");
            msg.q_list.clear();
            assert_eq!(msg.to_bytes().unwrap(), [0xf0, 0x7f, 17, 2, 16, code, 0xf7]);
            msg.q_list = "36.6".into();
        }
    }

    #[test]
    fn msc_writes_address_command_ascii_fields_and_terminator() {
        let mut msg = example();
        assert_eq!(
            msg.to_bytes().unwrap(),
            [
                0xf0, 0x7f, 17, 2, 16, 1, 0x32, 0x33, 0x35, 0x2e, 0x36, 0, 0x33, 0x36, 0x2e, 0x36,
                0, 0x35, 0x39, 0xf7
            ]
        );
        msg.command = MscCommand::Stop;
        msg.q_path.clear();
        assert_eq!(
            msg.to_bytes().unwrap(),
            [
                0xf0, 0x7f, 17, 2, 16, 2, 0x32, 0x33, 0x35, 0x2e, 0x36, 0, 0x33, 0x36, 0x2e, 0x36,
                0xf7
            ]
        );
        msg.command = MscCommand::Resume;
        msg.q_list.clear();
        assert_eq!(
            msg.to_bytes().unwrap(),
            [0xf0, 0x7f, 17, 2, 16, 3, 0x32, 0x33, 0x35, 0x2e, 0x36, 0xf7]
        );
        msg.q_number.clear();
        assert_eq!(msg.to_bytes().unwrap(), [0xf0, 0x7f, 17, 2, 16, 3, 0xf7]);
    }

    #[test]
    fn msc_rejects_unaddressed_invalid_and_oversize_messages() {
        for id in [112, 126, 127, 255] {
            assert!(MscMessage {
                device_id: id,
                ..example()
            }
            .to_bytes()
            .is_err());
        }
        assert!(MscMessage {
            command_format: 0,
            ..example()
        }
        .to_bytes()
        .is_err());
        for invalid in ["", "1..2", "1A", "１", ".1", "2.", "1\0"] {
            assert!(MscMessage {
                q_number: invalid.into(),
                ..example()
            }
            .to_bytes()
            .is_err());
        }
        assert!(MscMessage {
            q_list: String::new(),
            ..example()
        }
        .to_bytes()
        .is_err());
        let mut msg = MscMessage {
            q_number: "1".repeat(121),
            q_list: String::new(),
            q_path: String::new(),
            ..example()
        };
        assert_eq!(msg.to_bytes().unwrap().len(), 128);
        msg.q_number.push('1');
        assert!(msg.to_bytes().is_err());
    }
}
