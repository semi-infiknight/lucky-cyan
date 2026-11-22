//! Portable MIDI voice bindings for workspace transport.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::midi_trigger::{MidiTrigger, MidiTriggerType};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MidiTransportAction {
    Go,
    PauseAll,
    ResumeAll,
    StopAll,
    HardStop,
    NextStandby,
    PreviousStandby,
    NextSequence,
    PreviousSequence,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MidiValueMatch {
    #[default]
    Exactly,
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct MidiTransportBinding {
    pub trigger: MidiTrigger,
    #[serde(default)]
    pub value_match: MidiValueMatch,
}

impl MidiTransportBinding {
    fn range(self) -> Option<(u8, u8)> {
        use MidiValueMatch::*;
        let minimum = u8::from(self.trigger.message_type == MidiTriggerType::NoteOn);
        let (low, high) = match (self.value_match, self.trigger.data2) {
            (_, None) => (minimum, 127),
            (Exactly, Some(value)) => (value, value),
            (Above, Some(value)) => (value.checked_add(1)?, 127),
            (Below, Some(value)) => (minimum, value.checked_sub(1)?),
        };
        let low = low.max(minimum);
        let high = high.min(127);
        (low <= high).then_some((low, high))
    }

    fn matches(self, bytes: &[u8], incoming: MidiTrigger) -> bool {
        if self.trigger.message_type != incoming.message_type
            || self.trigger.data1 != incoming.data1
            || (self.trigger.channel != 0 && self.trigger.channel != incoming.channel)
        {
            return false;
        }
        incoming.message_type == MidiTriggerType::ProgramChange
            || self.range().is_some_and(|(low, high)| bytes.get(2).is_some_and(|value| (low..=high).contains(value)))
    }

    fn overlaps(self, other: Self) -> bool {
        if self.trigger.message_type != other.trigger.message_type
            || self.trigger.data1 != other.trigger.data1
            || (self.trigger.channel != 0 && other.trigger.channel != 0 && self.trigger.channel != other.trigger.channel)
        {
            return false;
        }
        self.range().zip(other.range()).is_some_and(|((low, high), (other_low, other_high))| {
            low <= other_high && other_low <= high
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct MidiWorkspaceControls {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bindings: BTreeMap<MidiTransportAction, MidiTransportBinding>,
}

impl MidiWorkspaceControls {
    pub fn validate(&self) -> Result<(), String> {
        for (action, binding) in &self.bindings {
            let trigger = binding.trigger;
            if trigger.channel > 16 || trigger.data1 > 127 || trigger.data2.is_some_and(|value| value > 127)
                || (trigger.message_type == MidiTriggerType::ProgramChange && trigger.data2.is_some())
                || binding.range().is_none()
            {
                return Err("Use a valid MIDI channel, message number and value range.".into());
            }
            if self.bindings.range(..*action).any(|(_, other)| binding.overlaps(*other)) {
                return Err("Two transport bindings match the same MIDI message. Choose a different number, channel or value range.".into());
            }
        }
        Ok(())
    }

    pub fn action(&self, bytes: &[u8]) -> Option<MidiTransportAction> {
        if !self.enabled { return None; }
        let received = MidiTrigger::from_message(bytes)?;
        let expected_len = if received.message_type == MidiTriggerType::ProgramChange { 2 } else { 3 };
        if bytes.len() != expected_len || bytes[1..].iter().any(|&byte| byte >= 128) {
            return None;
        }
        self.bindings.iter().find(|(_, binding)| binding.matches(bytes, received))
            .map(|(&action, _)| action)
    }
}
