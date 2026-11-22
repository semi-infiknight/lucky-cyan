use serde::{Deserialize, Serialize};

use crate::engine::{
    device_manager::{NamedOutput, OutputPatch},
    voice::MATRIX_OUTPUTS,
};

use super::point_mix::AudioMapPatch;

/// File Audio cue compensation tied to one exact, one-to-one Output Patch.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputTrim {
    pub patch: AudioMapPatch,
    pub levels_db: Vec<f64>,
}

impl OutputTrim {
    pub fn valid_channels(channels: &[u16]) -> bool {
        let mut seen = [false; MATRIX_OUTPUTS];
        !channels.is_empty()
            && channels.iter().all(|&channel| {
                seen.get_mut(usize::from(channel))
                    .is_some_and(|assigned| !std::mem::replace(assigned, true))
            })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.patch.id.is_none()
            || self.patch.device_id.as_ref().is_none_or(String::is_empty)
            || !Self::valid_channels(&self.patch.channels)
            || self.levels_db.len() != self.patch.channels.len()
            || self.levels_db.iter().any(|db| !(-60.0..=12.0).contains(db))
        {
            return Err(
                "Output trim needs one level between −60 and +12 dB per assigned channel.".into(),
            );
        }
        Ok(())
    }

    pub fn route(&self, patch: Option<&OutputPatch>) -> Result<NamedOutput, String> {
        let patch = patch.filter(|patch| Self::valid_channels(&patch.channels)
                && AudioMapPatch::from_output_patch(patch) == self.patch)
            .ok_or("Output trim belongs to a different channel assignment. Review it in the Trim tab before GO.")?;
        patch.named_output()?.ok_or_else(|| {
            "Output trim needs an Output Patch set to require its named device.".into()
        })
    }

    pub fn linear_levels(&self) -> Vec<f32> {
        self.levels_db
            .iter()
            .map(|&db| super::types::db_to_linear(db) as f32)
            .collect()
    }
}
