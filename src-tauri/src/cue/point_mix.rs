//! Bake a stationary sound source into one row of an Audio cue's routing matrix.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Speaker {
    /// One-based logical output of the cue patch.
    pub output: usize,
    #[serde(flatten)]
    pub position: Position,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PointMix {
    /// One-based file input.
    pub input: usize,
    pub source: Position,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speakers: Vec<Speaker>,
    /// The exact version that supplied the baked routes. Editing a workspace
    /// map cannot change this snapshot or the cue's saved matrix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<AudioMap>,
}

/// One workspace calibration tied to the output patch it was prepared for.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AudioMap {
    pub id: Uuid,
    pub revision: Uuid,
    pub name: String,
    pub patch: AudioMapPatch,
    pub marks: Vec<AudioMark>,
}

/// Exact logical-to-physical output assignment at calibration time.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AudioMapPatch {
    pub id: Option<Uuid>,
    pub device_id: Option<String>,
    pub channels: Vec<u16>,
}

impl AudioMapPatch {
    /// The unpatched Audio cue's two logical outputs.
    pub fn stereo() -> Self {
        Self {
            id: None,
            device_id: None,
            channels: vec![0, 1],
        }
    }

    /// Only the engine's supported matrix outputs can take a calibration.
    pub fn from_output_patch(patch: &crate::engine::device_manager::OutputPatch) -> Self {
        Self {
            id: Some(patch.id),
            device_id: Some(patch.device_id.clone()),
            channels: patch
                .channels
                .iter()
                .take(crate::engine::voice::MATRIX_OUTPUTS)
                .copied()
                .collect(),
        }
    }
}

/// The exact mix to use when a source is placed at this position.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AudioMark {
    pub id: Uuid,
    pub name: String,
    pub position: Position,
    /// One level per logical output, with -60 dB as the matrix silence floor.
    pub levels: Vec<f64>,
}

impl AudioMap {
    /// Reject ambiguous marks and calibrations outside the matrix limits.
    pub fn validate(&self) -> Result<(), String> {
        let width = self.patch.channels.len();
        if self.name.trim().is_empty() || self.name.chars().count() > 100 {
            return Err("Give the audio map a name of up to 100 characters.".into());
        }
        if !(1..=crate::engine::voice::MATRIX_OUTPUTS).contains(&width) {
            return Err("The output patch needs at least one logical output.".into());
        }
        if !(2..=128).contains(&self.marks.len()) {
            return Err("Add between 2 and 128 calibration marks.".into());
        }
        let mut names = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for mark in &self.marks {
            let name = mark.name.trim();
            if name.is_empty() || name.chars().count() > 100 || !names.insert(name.to_lowercase()) {
                return Err("Give each mark a different name of up to 100 characters.".into());
            }
            if !ids.insert(mark.id) {
                return Err("Each mark needs its own ID.".into());
            }
            if !mark.position.valid() {
                return Err("Enter mark positions between 0 and 100 on both axes.".into());
            }
            if mark.levels.len() != width
                || mark.levels.iter().any(|db| !(-60.0..=24.0).contains(db))
            {
                return Err(
                    "Set one calibration level between −60 and 24 dB per logical output.".into(),
                );
            }
        }
        Ok(())
    }

    fn levels_at(&self, source: &Position, outputs: usize) -> Result<Vec<f64>, String> {
        self.validate()?;
        if self.patch.channels.len() != outputs {
            return Err("This map was calibrated for a different output width.".into());
        }
        let distances = self
            .marks
            .iter()
            .map(|mark| mark.position.distance_to(source))
            .collect::<Vec<_>>();
        let mut exact = self
            .marks
            .iter()
            .zip(&distances)
            .filter(|(_, distance)| **distance == 0.0);
        if let (Some((mark, _)), None) = (exact.next(), exact.next()) {
            return Ok(mark.levels.clone());
        }
        let weights = distance_weights(&distances);
        let power_weight: f64 = weights.iter().map(|weight| weight * weight).sum();
        let mut powers = vec![0.0; outputs];
        for (mark, weight) in self.marks.iter().zip(weights) {
            let fraction = weight * weight / power_weight;
            for (power, &db) in powers.iter_mut().zip(&mark.levels) {
                if db > -60.0 {
                    *power += fraction * 10.0_f64.powf(db / 10.0);
                }
            }
        }
        Ok(powers
            .into_iter()
            .map(|power| (10.0 * power.log10()).clamp(-60.0, 24.0))
            .collect())
    }
}

#[cfg(test)]
pub(crate) fn stereo_calibration() -> AudioMap {
    AudioMap {
        id: Uuid::new_v4(),
        revision: Uuid::new_v4(),
        name: "Theatre".into(),
        patch: AudioMapPatch::stereo(),
        marks: vec![
            AudioMark {
                id: Uuid::new_v4(),
                name: "Prop radio".into(),
                position: Position { x: 0.0, y: 50.0 },
                levels: vec![0.0, -12.0],
            },
            AudioMark {
                id: Uuid::new_v4(),
                name: "Hallway".into(),
                position: Position { x: 100.0, y: 50.0 },
                levels: vec![-60.0, -6.0],
            },
        ],
    }
}

impl Position {
    fn valid(&self) -> bool {
        (0.0..=100.0).contains(&self.x) && (0.0..=100.0).contains(&self.y)
    }

    fn distance_to(&self, other: &Self) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

fn distance_weights(distances: &[f64]) -> Vec<f64> {
    let nearest = distances.iter().copied().fold(f64::INFINITY, f64::min);
    distances
        .iter()
        .map(|&d| if d == 0.0 { 1.0 } else { nearest / d })
        .collect()
}

impl PointMix {
    /// Inverse-distance weights normalized to unit power. At a speaker's
    /// exact position its output receives unity. Colocated speakers share it.
    pub fn levels(&self, outputs: usize) -> Result<Vec<f64>, String> {
        let limit = crate::engine::voice::MATRIX_OUTPUTS;
        if !(1..=crate::engine::voice::MATRIX_INPUTS).contains(&self.input) {
            return Err("Select a file input between 1 and 128.".into());
        }
        if !self.source.valid() {
            return Err("Enter positions between 0 and 100 on both axes.".into());
        }
        if let Some(map) = &self.map {
            return map.levels_at(&self.source, outputs);
        }
        if self.speakers.iter().any(|s| !s.position.valid()) {
            return Err("Enter positions between 0 and 100 on both axes.".into());
        }
        if self.speakers.len() < 2 || outputs > limit || self.speakers.len() > outputs {
            return Err("Assign at least two different outputs from the current patch.".into());
        }
        let mut used = vec![false; outputs];
        let mut distances = Vec::with_capacity(self.speakers.len());
        for speaker in &self.speakers {
            if !(1..=outputs).contains(&speaker.output) || used[speaker.output - 1] {
                return Err("Each speaker needs a different output in the current patch.".into());
            }
            used[speaker.output - 1] = true;
            distances.push(speaker.position.distance_to(&self.source));
        }
        let weights = distance_weights(&distances);
        let norm = weights.iter().map(|w| w * w).sum::<f64>().sqrt();
        let mut levels = vec![-60.0; outputs];
        for (speaker, weight) in self.speakers.iter().zip(weights) {
            levels[speaker.output - 1] = (20.0 * (weight / norm).log10()).max(-60.0);
        }
        Ok(levels)
    }

    pub fn apply(&self, matrix: &mut Vec<Vec<f64>>, outputs: usize) -> Result<(), String> {
        let levels = self.levels(outputs)?;
        if matrix.len() < self.input {
            matrix.resize_with(self.input, Vec::new);
        }
        matrix[self.input - 1] = levels;
        Ok(())
    }
}

/// References one pair of marks on the exact workspace calibration that was
/// applied to the target cue. The target retains its independently saved mix.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MapMotion {
    pub map_id: Uuid,
    pub revision: Uuid,
    pub start_mark: Uuid,
    pub end_mark: Uuid,
}

impl MapMotion {
    pub(crate) fn valid(self) -> bool { self.start_mark != self.end_mark }
}

pub struct MapMotionRun {
    voice: Uuid,
    owner: Uuid,
    patch_id: Uuid,
    start: Position,
    end: Position,
    point: PointMix,
    levels: Vec<Vec<f64>>,
    mirror_mono: bool,
}

impl MapMotionRun {
    pub(crate) fn bind(
        motion: MapMotion, target: &dyn super::traits::Cue, context: &super::context::CueContext,
    ) -> Result<Self, String> {
        let mut point = target.applied_point_mix().cloned()
            .ok_or("Apply a saved audio map to the Audio cue first.")?;
        let map = point.map.as_ref().ok_or("The Audio cue uses no saved map.")?;
        if (map.id, map.revision) != (motion.map_id, motion.revision) || !motion.valid() {
            return Err("Choose two different marks from the exact map applied to the Audio cue.".into());
        }
        let patch_id = target.output_patch_id().ok_or("Assign a named output patch explicitly to the Audio cue.")?;
        if !Self::calibration_matches(context, map, patch_id) {
            return Err("The audio calibration or its output assignment changed. Reapply the current map while stopped.".into());
        }
        let find = |id| map.marks.iter().find(|mark| mark.id == id).map(|mark| mark.position.clone());
        let start = find(motion.start_mark).ok_or("The start mark is missing.")?;
        let end = find(motion.end_mark).ok_or("The destination mark is missing.")?;
        if start == end {
            return Err("Choose marks at two different positions.".into());
        }
        let live = target.live_audio_params().ok_or("Start the target Audio cue first.")?;
        if let Some(route) = context.audio_engine.named_voice_output(live.voice_id) {
            if !context.resolve_patch(Some(patch_id)).is_some_and(|patch| route.matches_patch(patch)) {
                return Err("The playing cue uses a different output assignment. Stop it, reapply the map, and start the cue again.".into());
            }
        }
        let mut levels = live.level_matrix.ok_or("Apply the map before starting the Audio cue.")?;
        let device = map.patch.device_id.as_deref().filter(|id| !id.is_empty())
            .ok_or("Calibrate a named output device for movement.")?;
        let channels = &map.patch.channels;
        point.source = start.clone();
        point.apply(&mut levels, channels.len())?;
        let expected = super::audio_cue::build_level_matrix(&levels, channels, live.matrix_mirror_mono)
            .ok_or("The starting route has no matrix.")?;
        let owner = Uuid::new_v4();
        if !context.audio_engine.claim_map_motion(live.voice_id, device, owner, &expected) {
            return Err("Match the start mark on the running Audio cue and use its named main output device.".into());
        }
        Ok(Self { voice: live.voice_id, owner, patch_id, start, end, point, levels, mirror_mono: live.matrix_mirror_mono })
    }

    fn calibration_matches(context: &super::context::CueContext, map: &AudioMap, patch_id: Uuid) -> bool {
        context.map_revisions.contains(&(map.id, map.revision))
            && context.resolve_patch(Some(patch_id)).is_some_and(|patch| AudioMapPatch::from_output_patch(patch) == map.patch)
    }

    pub(crate) fn update(&mut self, context: &super::context::CueContext, progress: f64) -> bool {
        let Some(map) = self.point.map.as_ref() else { return false };
        if !Self::calibration_matches(context, map, self.patch_id) { return false; }
        self.point.source = self.start.toward(&self.end, progress);
        if self.point.apply(&mut self.levels, map.patch.channels.len()).is_err() { return false; }
        let Some(matrix) = super::audio_cue::build_level_matrix(&self.levels, &map.patch.channels, self.mirror_mono) else {
            return false;
        };
        context.audio_engine.update_map_motion(self.voice, map.patch.device_id.as_deref().unwrap_or_default(), self.owner, matrix)
    }
}

impl Position {
    fn toward(&self, end: &Self, progress: f64) -> Self {
        if progress <= 0.0 { return self.clone(); }
        if progress >= 1.0 { return end.clone(); }
        Self { x: self.x + (end.x - self.x) * progress, y: self.y + (end.y - self.y) * progress }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(source_x: f64) -> PointMix {
        PointMix {
            input: 2,
            source: Position {
                x: source_x,
                y: 50.0,
            },
            speakers: vec![
                Speaker {
                    output: 2,
                    position: Position { x: 0.0, y: 50.0 },
                },
                Speaker {
                    output: 4,
                    position: Position { x: 100.0, y: 50.0 },
                },
            ],
            map: None,
        }
    }

    #[test]
    fn point_moves_continuously_between_exact_speaker_endpoints() {
        let mut previous = 1.0;
        for x in 0..=100 {
            let levels = stereo(x as f64).levels(4).unwrap();
            assert_eq!((levels[0], levels[2]), (-60.0, -60.0));
            let gain = |db| {
                if db == -60.0 {
                    0.0
                } else {
                    10.0_f64.powf(db / 20.0)
                }
            };
            let (left, right) = (gain(levels[1]), gain(levels[3]));
            assert!(left <= previous && previous - left < 0.022);
            assert!((left * left + right * right - 1.0).abs() < 1e-10);
            previous = left;
        }
        assert_eq!(stereo(100.0).levels(4).unwrap(), [-60.0, -60.0, -60.0, 0.0]);
        let mut coincident = stereo(0.0);
        coincident.speakers[1].position.x = 0.0;
        let shared = coincident.levels(4).unwrap();
        assert_eq!(shared[1], shared[3]);
        assert!((shared[1] + 3.0102999566).abs() < 1e-8);
    }

    #[test]
    fn rejected_layout_never_changes_existing_matrix() {
        let mut invalid = stereo(60.0);
        invalid.speakers[1].output = 2;
        let mut matrix = vec![vec![0.0]];
        assert!(invalid.apply(&mut matrix, 4).is_err());
        assert_eq!(matrix, vec![vec![0.0]]);
        assert!(stereo(f64::NAN).levels(4).is_err());
        assert!(stereo(0.0).levels(1).is_err());
        let mut valid = stereo(25.0);
        valid.input = 128;
        valid.apply(&mut matrix, 4).unwrap();
        assert_eq!(matrix[0], [0.0]);
        assert_eq!(matrix[127], valid.levels(4).unwrap());
    }

    #[test]
    fn calibrated_marks_keep_exact_mixes_and_interpolate_in_power() {
        let mut map = stereo_calibration();
        for mark in &map.marks {
            assert_eq!(map.levels_at(&mark.position, 2).unwrap(), mark.levels);
        }
        let source = Position { x: 50.0, y: 50.0 };
        let levels = map.levels_at(&source, 2).unwrap();
        let right_power = (10.0_f64.powf(-1.2) + 10.0_f64.powf(-0.6)) * 0.5;
        assert!((levels[0] - 10.0 * 0.5_f64.log10()).abs() < 1e-10);
        assert!((levels[1] - 10.0 * right_power.log10()).abs() < 1e-10);
        assert!(map.levels_at(&source, 3).is_err());
        for mark in &mut map.marks {
            mark.position = source.clone();
        }
        assert_eq!(map.levels_at(&source, 2).unwrap(), levels);
        map.marks[0].levels.pop();
        assert!(map.levels_at(&source, 2).is_err());
    }
}
