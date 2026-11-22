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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MscAction {
    Go(Option<String>),
    Pause(String),
    Resume(Option<String>),
    Load(String),
    AllOff,
    Step { forward: bool, sequence: bool },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MscTransport {
    pub action: MscAction,
    pub list: Option<String>,
}

impl MscTransport {
    pub(crate) fn decode(bytes: &[u8], destination: u8) -> Option<Self> {
        if bytes.len() > 128 {
            return None;
        }
        let [0xf0, 0x7f, device, 0x02, 0x01 | 0x10 | 0x30, command, payload @ .., 0xf7] = bytes else {
            return None;
        };
        if destination > 126 || (*device != 127 && *device != destination) {
            return None;
        }
        let command = MscCommand::from_code(u64::from(*command))?;
        if matches!(command, MscCommand::StandbyNext | MscCommand::StandbyPrevious
            | MscCommand::SequenceNext | MscCommand::SequencePrevious)
        {
            let list = if payload.is_empty() {
                None
            } else {
                Some(decode_address(payload.strip_suffix(&[0]).unwrap_or(payload))?)
            };
            let action = MscAction::Step {
                forward: matches!(command, MscCommand::StandbyNext | MscCommand::SequenceNext),
                sequence: matches!(command, MscCommand::SequenceNext | MscCommand::SequencePrevious),
            };
            return Some(Self { action, list });
        }
        if payload.is_empty() {
            let action = match command {
                MscCommand::Go => MscAction::Go(None),
                MscCommand::Resume => MscAction::Resume(None),
                MscCommand::AllOff => MscAction::AllOff,
                _ => return None,
            };
            return Some(Self { action, list: None });
        }

        let mut fields = payload.split(|&byte| byte == 0);
        let number = decode_address(fields.next()?)?;
        let list = match fields.next() {
            Some(list) if !list.is_empty() => Some(decode_address(list)?),
            _ => None,
        };
        // A trailing delimiter may omit the path. Hierarchical paths have
        // their own addressing rules and cannot fall through to a list match.
        if fields.next().is_some_and(|path| !path.is_empty()) || fields.next().is_some() {
            return None;
        }
        let action = match command {
            MscCommand::Go => MscAction::Go(Some(number)),
            MscCommand::Stop => MscAction::Pause(number),
            MscCommand::Resume => MscAction::Resume(Some(number)),
            MscCommand::Load => MscAction::Load(number),
            _ => return None,
        };
        Some(Self { action, list })
    }
}

fn decode_address(bytes: &[u8]) -> Option<String> {
    let number = std::str::from_utf8(bytes).ok()?;
    valid_cue_number(number).then(|| number.to_owned())
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
        let unscoped = |action| Some(MscTransport { action, list: None });
        assert_eq!(MscTransport::decode(&frame(7, 16, 1, b"01.2"), 7),
            unscoped(MscAction::Go(Some("01.2".into()))));
        assert_eq!(MscTransport::decode(&frame(127, 48, 2, b"01.2\0"), 7),
            unscoped(MscAction::Pause("01.2".into())));
        assert_eq!(MscTransport::decode(&frame(7, 1, 3, b""), 7), unscoped(MscAction::Resume(None)));
        assert_eq!(MscTransport::decode(&frame(7, 16, 8, b""), 7), unscoped(MscAction::AllOff));
        assert_eq!(MscTransport::decode(&frame(7, 16, 1, b"1\x0004\x00"), 7),
            Some(MscTransport { action: MscAction::Go(Some("1".into())), list: Some("04".into()) }));
        assert_eq!(MscTransport::decode(&frame(7, 16, 20, b"04"), 7),
            Some(MscTransport { action: MscAction::Step { forward: false, sequence: true }, list: Some("04".into()) }));
        for rejected in [
            frame(8, 16, 1, b"1"),
            frame(7, 17, 1, b"1"),
            frame(7, 16, 1, b"1\x00\x004"),
            frame(7, 16, 1, b"1\x004\x005"),
            frame(7, 16, 17, b"1\x004"),
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
