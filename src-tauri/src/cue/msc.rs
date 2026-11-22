//! Addressed MIDI Show Control 1.1 transport commands.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MscCommand {
    Go,
    Stop,
    Resume,
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
        let command = match self.command {
            MscCommand::Go => 1,
            MscCommand::Stop => 2,
            MscCommand::Resume => 3,
        };
        let mut bytes = vec![
            0xf0,
            0x7f,
            self.device_id,
            0x02,
            self.command_format,
            command,
        ];
        let mut empty_field = false;
        for (index, field) in [&self.q_number, &self.q_list, &self.q_path]
            .into_iter()
            .enumerate()
        {
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
            if !field
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()))
            {
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
