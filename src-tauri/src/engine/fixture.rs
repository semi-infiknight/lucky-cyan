//! Fixture model for the lighting patch.
//!
//! A [`PatchedFixture`] is a named lighting instrument placed at a DMX address
//! in a universe.  It embeds its [`FixtureType`] (the channel layout) so the
//! workspace is fully self-contained — no cross-references to resolve and no
//! external library to ship.  [`builtin_fixture_types`] provides ready-made
//! templates the operator picks from when patching; once patched, each
//! fixture's parameters can be tweaked independently.
//!
//! A Light Cue references a fixture by ID and a parameter by index; at GO it
//! resolves `(universe, channel, width)` from the patch and submits a fade to
//! the [`super::dmx_engine::DmxEngine`].

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::dmx_engine::ChannelWidth;
use super::dmx_sink::DMX_UNIVERSE_SIZE;

/// Number of DMX slots a [`ChannelWidth`] occupies.
pub fn width_channels(width: ChannelWidth) -> u16 {
    match width {
        ChannelWidth::Bit8 => 1,
        ChannelWidth::Bit16 => 2,
    }
}

/// What a fixture parameter controls.  Drives the patch UI grouping, the
/// identify ("test fixture") value, and future colour-mixing helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    /// Master dimmer / intensity.
    Intensity,
    Red,
    Green,
    Blue,
    Cyan,
    Magenta,
    Yellow,
    Hue,
    Saturation,
    /// The native HSI color intensity channel, separate from a master dimmer.
    HsiIntensity,
    /// The native HSV value channel, separate from HSI intensity and the dimmer.
    HsvValue,
    White,
    Amber,
    /// Ultraviolet.
    Uv,
    /// Horizontal position of a moving head.
    Pan,
    /// Vertical position of a moving head.
    Tilt,
    /// Anything else (gobo, strobe, focus, …) — set by raw value.
    Generic,
}

impl ParamKind {
    /// The normalised value to drive this parameter to when identifying the
    /// fixture (the "test fixture" button): colour + intensity go full, moving
    /// heads centre so the beam is visible, everything else stays dark.
    pub fn identify_value(self) -> f64 {
        match self {
            ParamKind::Intensity
            | ParamKind::Red
            | ParamKind::Green
            | ParamKind::Blue
            | ParamKind::White
            | ParamKind::Amber
            | ParamKind::Uv
            | ParamKind::HsvValue
            | ParamKind::HsiIntensity => 1.0,
            ParamKind::Pan | ParamKind::Tilt => 0.5,
            // Open the subtractive filters or use an unsaturated beam for Identify.
            ParamKind::Cyan
            | ParamKind::Magenta
            | ParamKind::Yellow
            | ParamKind::Hue
            | ParamKind::Saturation
            | ParamKind::Generic => 0.0,
        }
    }
}

/// One controllable parameter of a fixture, at a fixed offset from the
/// fixture's base address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureParam {
    /// What this parameter controls.
    pub kind: ParamKind,
    /// Human-readable label shown in the inspector (e.g. `"Dimmer"`, `"Red"`).
    pub name: String,
    /// Zero-based offset of this parameter's (coarse) channel from the
    /// fixture's base address.
    pub channel_offset: u16,
    /// Whether this parameter is 8- or 16-bit on the wire.
    pub width: ChannelWidth,
    /// Default normalised value `[0, 1]`.
    pub default: f64,
}

/// The channel layout of a kind of lighting instrument.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureType {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manufacturer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    /// Display name (e.g. `"RGB PAR"`, `"Moving Head 16-bit"`).
    pub name: String,
    /// The parameters, in display order.
    pub parameters: Vec<FixtureParam>,
}

impl FixtureType {
    /// Number of DMX channels this fixture occupies, i.e. the highest
    /// `channel_offset + width` across its parameters.
    pub fn footprint(&self) -> u16 {
        self.parameters
            .iter()
            .map(|p| p.channel_offset + width_channels(p.width))
            .max()
            .unwrap_or(0)
    }
}

/// A fixture placed at a DMX address in the workspace patch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchedFixture {
    /// Unique identifier, referenced by Light Cue targets.
    pub id: Uuid,
    /// Operator-facing label (e.g. `"Front wash L"`).
    pub label: String,
    /// Logical universe this fixture lives in.
    pub universe: u16,
    /// One-based DMX start address of the fixture's first channel.
    pub base_address: u16,
    /// The embedded channel layout.
    pub fixture_type: FixtureType,
}

/// An exact physical channel assignment used by the live color controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchedChannel {
    pub universe: u16,
    /// Zero-based coarse channel within the universe.
    pub channel: u16,
    pub width: ChannelWidth,
}

/// The ordered color channels actually assigned to one fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureColorChannels {
    pub id: Uuid,
    pub channels: [PatchedChannel; 3],
}

impl PatchedChannel {
    fn slots(self) -> impl Iterator<Item = (u16, u16)> {
        (self.channel..self.channel + width_channels(self.width))
            .map(move |channel| (self.universe, channel))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureColorModel {
    Cmy,
    Hsi,
    Hsv,
}

impl FixtureColorModel {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Cmy => "CMY",
            Self::Hsi => "HSI",
            Self::Hsv => "HSV",
        }
    }

    fn roles(self) -> [ParamKind; 3] {
        use ParamKind::*;
        match self {
            Self::Cmy => [Cyan, Magenta, Yellow],
            Self::Hsi => [Hue, Saturation, HsiIntensity],
            Self::Hsv => [Hue, Saturation, HsvValue],
        }
    }
}

impl PatchedFixture {
    /// Create a fixture from a type at the given address with a fresh UUID.
    pub fn new(label: impl Into<String>, universe: u16, base_address: u16, fixture_type: FixtureType) -> Self {
        Self {
            id: Uuid::new_v4(),
            label: label.into(),
            universe,
            base_address,
            fixture_type,
        }
    }

    /// Inclusive one-based address range `(first, last)` this fixture occupies.
    /// Returns `None` for a fixture with no parameters.
    pub fn address_span(&self) -> Option<(u16, u16)> {
        let footprint = self.fixture_type.footprint();
        if footprint == 0 {
            return None;
        }
        Some((self.base_address, self.base_address + footprint - 1))
    }

    /// Resolve a parameter index to its `(universe, zero-based channel, width)`
    /// for the DMX engine.  Returns `None` if the index is out of range.
    pub fn resolve_channel(&self, param_index: usize) -> Option<(u16, u16, ChannelWidth)> {
        let param = self.fixture_type.parameters.get(param_index)?;
        // base_address is 1-based; the engine addresses channels 0-based.
        let channel = self.base_address.checked_sub(1)?.checked_add(param.channel_offset)?;
        if channel.checked_add(width_channels(param.width))? as usize > DMX_UNIVERSE_SIZE {
            return None;
        }
        Some((self.universe, channel, param.width))
    }

    /// Find a native color triplet by explicit roles. Ambiguous roles and
    /// overlaps would let one absolute color component change another.
    pub(crate) fn color_channels(
        &self,
        model: FixtureColorModel,
    ) -> Result<[PatchedChannel; 3], String> {
        let params = &self.fixture_type.parameters;
        let other_hue_model = match model {
            FixtureColorModel::Cmy => None,
            FixtureColorModel::Hsi => Some(ParamKind::HsvValue),
            FixtureColorModel::Hsv => Some(ParamKind::HsiIntensity),
        };
        if other_hue_model.is_some_and(|role| params.iter().any(|param| param.kind == role)) {
            return Err("Choose either HSI intensity or HSV value for this hue and saturation pair".into());
        }
        let resolve = |role| {
            let mut matches = params.iter().enumerate().filter(|(_, param)| param.kind == role);
            let (index, _) = matches
                .next()
                .ok_or_else(|| format!("Assign all three {} roles in the fixture profile", model.name()))?;
            if matches.next().is_some() {
                return Err(format!("Assign exactly one channel for each {} role", model.name()));
            }
            let (universe, channel, width) = self
                .resolve_channel(index)
                .ok_or_else(|| format!("{} parameter is outside the universe", model.name()))?;
            let end = channel + width_channels(width);
            for other_index in (0..params.len()).filter(|&other| other != index) {
                let other = &params[other_index];
                let Some(start) = self.base_address.checked_sub(1).and_then(|base| base.checked_add(other.channel_offset)) else { continue; };
                if start < end && start.saturating_add(width_channels(other.width)) > channel {
                    return Err(format!("{} channels overlap another fixture parameter", model.name()));
                }
            }
            Ok(PatchedChannel { universe, channel, width })
        };
        let [first, second, third] = model.roles();
        Ok([resolve(first)?, resolve(second)?, resolve(third)?])
    }
}

/// A named set of fixtures driven together by a Light Cue: one colour /
/// intensity control fans out to every member.  Members are referenced by ID so
/// a group survives a fixture being re-patched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureGroup {
    /// Unique identifier, referenced by Light Cue group targets.
    pub id: Uuid,
    /// Operator-facing label (e.g. `"Front wash"`).
    pub label: String,
    /// Member fixture IDs.
    pub fixture_ids: Vec<Uuid>,
}

impl FixtureGroup {
    /// Create a group with a fresh UUID.
    pub fn new(label: impl Into<String>, fixture_ids: Vec<Uuid>) -> Self {
        Self { id: Uuid::new_v4(), label: label.into(), fixture_ids }
    }

    /// Resolve the complete native color triplet of every member before any output.
    pub(crate) fn color_channels(
        &self,
        fixtures: &[PatchedFixture],
        model: FixtureColorModel,
    ) -> Result<Vec<FixtureColorChannels>, String> {
        if self.fixture_ids.is_empty() {
            return Err("The fixture group has no members.".into());
        }
        let mut seen = HashSet::new();
        let mut mapped = Vec::with_capacity(self.fixture_ids.len());
        for id in &self.fixture_ids {
            if !seen.insert(id) {
                return Err("The fixture group repeats a member.".into());
            }
            let fixture = fixtures.iter().find(|fixture| fixture.id == *id)
                .ok_or("A member of the fixture group is no longer patched.")?;
            let channels = fixture.color_channels(model)
                .map_err(|error| format!("{}: {error}", fixture.label))?;
            mapped.push(FixtureColorChannels { id: *id, channels });
        }
        check_color_patch(fixtures, &mapped, model)?;
        Ok(mapped)
    }
}

/// Ensure the selected color slots cannot also affect another fixture parameter.
pub(crate) fn check_color_patch(
    fixtures: &[PatchedFixture],
    mapped: &[FixtureColorChannels],
    model: FixtureColorModel,
) -> Result<(), String> {
    let mut slots = HashMap::new();
    for &FixtureColorChannels { id, channels } in mapped {
        for slot in channels.into_iter().flat_map(PatchedChannel::slots) {
            if slots.insert(slot, id).is_some() {
                return Err(format!("{} channels overlap between group members.", model.name()));
            }
        }
    }
    for fixture in fixtures {
        for index in 0..fixture.fixture_type.parameters.len() {
            let Some((universe, channel, width)) = fixture.resolve_channel(index) else { continue; };
            let physical = PatchedChannel { universe, channel, width };
            if physical.slots().any(|slot| slots.get(&slot).is_some_and(|id| *id != fixture.id)) {
                return Err(format!("A {} channel overlaps {} in the patch", model.name(), fixture.label));
            }
        }
    }
    Ok(())
}

/// A detected address clash between two patched fixtures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureConflict {
    pub fixture_a: Uuid,
    pub fixture_b: Uuid,
    pub universe: u16,
    /// Human-readable description for the patch UI.
    pub message: String,
}

/// Whether two fixtures share any DMX channel in the same universe.
fn overlaps(a: &PatchedFixture, b: &PatchedFixture) -> bool {
    if a.universe != b.universe {
        return false;
    }
    let (Some((a0, a1)), Some((b0, b1))) = (a.address_span(), b.address_span()) else {
        return false;
    };
    a0 <= b1 && b0 <= a1
}

/// Find every pair of fixtures whose channel ranges overlap.
pub fn find_conflicts(fixtures: &[PatchedFixture]) -> Vec<FixtureConflict> {
    let mut conflicts = Vec::new();
    for i in 0..fixtures.len() {
        for j in (i + 1)..fixtures.len() {
            let a = &fixtures[i];
            let b = &fixtures[j];
            if overlaps(a, b) {
                conflicts.push(FixtureConflict {
                    fixture_a: a.id,
                    fixture_b: b.id,
                    universe: a.universe,
                    message: format!(
                        "“{}” and “{}” overlap on universe {}",
                        a.label, b.label, a.universe
                    ),
                });
            }
        }
    }
    conflicts
}

// ---------------------------------------------------------------------------
// Built-in templates
// ---------------------------------------------------------------------------

fn param(kind: ParamKind, name: &str, offset: u16, width: ChannelWidth, default: f64) -> FixtureParam {
    FixtureParam { kind, name: name.to_string(), channel_offset: offset, width, default }
}

/// The built-in fixture templates offered when patching.  These are pure
/// data — picking one copies its layout into a [`PatchedFixture`].
pub fn builtin_fixture_types() -> Vec<FixtureType> {
    use ChannelWidth::{Bit16, Bit8};
    use ParamKind::*;

    vec![
        FixtureType {
            manufacturer: String::new(),
            mode: String::new(),
            name: "Dimmer (1ch)".to_string(),
            parameters: vec![param(Intensity, "Dimmer", 0, Bit8, 0.0)],
        },
        FixtureType {
            manufacturer: String::new(),
            mode: String::new(),
            name: "RGB (3ch)".to_string(),
            parameters: vec![
                param(Red, "Red", 0, Bit8, 0.0),
                param(Green, "Green", 1, Bit8, 0.0),
                param(Blue, "Blue", 2, Bit8, 0.0),
            ],
        },
        FixtureType {
            manufacturer: String::new(),
            mode: String::new(),
            name: "RGBW (4ch)".to_string(),
            parameters: vec![
                param(Red, "Red", 0, Bit8, 0.0),
                param(Green, "Green", 1, Bit8, 0.0),
                param(Blue, "Blue", 2, Bit8, 0.0),
                param(White, "White", 3, Bit8, 0.0),
            ],
        },
        FixtureType {
            manufacturer: String::new(),
            mode: String::new(),
            name: "CMY (3ch)".to_string(),
            parameters: vec![
                param(Cyan, "Cyan", 0, Bit8, 0.0),
                param(Magenta, "Magenta", 1, Bit8, 0.0),
                param(Yellow, "Yellow", 2, Bit8, 0.0),
            ],
        },
        FixtureType {
            manufacturer: String::new(),
            mode: String::new(),
            name: "HSI (3ch)".to_string(),
            parameters: vec![
                param(Hue, "Hue", 0, Bit8, 0.0),
                param(Saturation, "Saturation", 1, Bit8, 0.0),
                param(HsiIntensity, "HSI intensity", 2, Bit8, 0.0),
            ],
        },
        FixtureType {
            manufacturer: String::new(),
            mode: String::new(),
            name: "HSV (3ch)".to_string(),
            parameters: vec![
                param(Hue, "Hue", 0, Bit8, 0.0),
                param(Saturation, "Saturation", 1, Bit8, 0.0),
                param(HsvValue, "HSV value", 2, Bit8, 0.0),
            ],
        },
        FixtureType {
            manufacturer: String::new(),
            mode: String::new(),
            name: "RGBA (4ch)".to_string(),
            parameters: vec![
                param(Red, "Red", 0, Bit8, 0.0),
                param(Green, "Green", 1, Bit8, 0.0),
                param(Blue, "Blue", 2, Bit8, 0.0),
                param(Amber, "Amber", 3, Bit8, 0.0),
            ],
        },
        FixtureType {
            manufacturer: String::new(),
            mode: String::new(),
            name: "PAR Dimmer+RGB (4ch)".to_string(),
            parameters: vec![
                param(Intensity, "Dimmer", 0, Bit8, 0.0),
                param(Red, "Red", 1, Bit8, 0.0),
                param(Green, "Green", 2, Bit8, 0.0),
                param(Blue, "Blue", 3, Bit8, 0.0),
            ],
        },
        FixtureType {
            manufacturer: String::new(),
            mode: String::new(),
            name: "Moving Head 16-bit (8ch)".to_string(),
            parameters: vec![
                param(Pan, "Pan", 0, Bit16, 0.5),
                param(Tilt, "Tilt", 2, Bit16, 0.5),
                param(Intensity, "Dimmer", 4, Bit8, 0.0),
                param(Red, "Red", 5, Bit8, 0.0),
                param(Green, "Green", 6, Bit8, 0.0),
                param(Blue, "Blue", 7, Bit8, 0.0),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb() -> FixtureType {
        builtin_fixture_types().into_iter().find(|t| t.name.starts_with("RGB ")).unwrap()
    }

    #[test]
    fn footprint_counts_16bit_as_two() {
        let mh = builtin_fixture_types()
            .into_iter()
            .find(|t| t.name.starts_with("Moving Head"))
            .unwrap();
        // Pan(2) + Tilt(2) + Dimmer + R + G + B = 8 channels.
        assert_eq!(mh.footprint(), 8);
    }

    #[test]
    fn resolve_channel_is_zero_based() {
        let f = PatchedFixture::new("Wash", 1, 10, rgb());
        // base 10 (1-based) → channel 9 (0-based); Green offset 1 → 10.
        assert_eq!(f.resolve_channel(0), Some((1, 9, ChannelWidth::Bit8)));
        assert_eq!(f.resolve_channel(1), Some((1, 10, ChannelWidth::Bit8)));
        assert_eq!(f.resolve_channel(2), Some((1, 11, ChannelWidth::Bit8)));
        assert_eq!(f.resolve_channel(3), None);
    }

    #[test]
    fn address_span_is_inclusive() {
        let f = PatchedFixture::new("Wash", 1, 10, rgb());
        assert_eq!(f.address_span(), Some((10, 12)));
    }

    #[test]
    fn conflicts_detect_overlap_same_universe_only() {
        let a = PatchedFixture::new("A", 1, 1, rgb()); // 1..3
        let b = PatchedFixture::new("B", 1, 3, rgb()); // 3..5 → overlaps A at 3
        let c = PatchedFixture::new("C", 1, 4, rgb()); // 4..6 → overlaps B, not A
        let d = PatchedFixture::new("D", 2, 1, rgb()); // other universe → no conflict

        let fixtures = vec![a, b, c, d];
        let conflicts = find_conflicts(&fixtures);
        // A–B and B–C overlap; A–C do not (1..3 vs 4..6); universe 2 never conflicts.
        assert_eq!(conflicts.len(), 2);
        assert!(conflicts.iter().all(|c| c.universe == 1));
    }

    #[test]
    fn adjacent_fixtures_do_not_conflict() {
        let a = PatchedFixture::new("A", 1, 1, rgb()); // 1..3
        let b = PatchedFixture::new("B", 1, 4, rgb()); // 4..6
        assert!(find_conflicts(&[a, b]).is_empty());
    }

    #[test]
    fn native_cmy_accepts_only_explicit_distinct_roles_with_valid_addresses() {
        use ParamKind::{Cyan, Intensity, Magenta, Yellow};
        let fixture_type = FixtureType {
            manufacturer: String::new(),
            mode: String::new(),
            name: "Example CMY".into(),
            parameters: vec![
                param(Intensity, "Dimmer", 0, ChannelWidth::Bit8, 0.0),
                param(Yellow, "Filter Z", 4, ChannelWidth::Bit8, 0.0),
                param(Cyan, "Filter X", 1, ChannelWidth::Bit16, 0.0),
                param(Magenta, "Filter Y", 3, ChannelWidth::Bit8, 0.0),
            ],
        };
        let mut fixture = PatchedFixture::new("Subtractive wash", 3, 20, fixture_type);
        let channels = fixture.color_channels(FixtureColorModel::Cmy).unwrap();
        assert_eq!(channels.map(|slot| slot.channel), [20, 22, 23]);
        assert_eq!(channels.map(|slot| slot.width), [ChannelWidth::Bit16, ChannelWidth::Bit8, ChannelWidth::Bit8]);
        assert_eq!(Cyan.identify_value(), 0.0);
        assert!(channels[0].slots().any(|slot| slot == (3, 21)));
        assert!(PatchedFixture::new("Additive wash", 3, 20, rgb()).color_channels(FixtureColorModel::Cmy).is_err());

        fixture.fixture_type.parameters[3].channel_offset = 2;
        assert!(fixture.color_channels(FixtureColorModel::Cmy).is_err());
        fixture.fixture_type.parameters[3].channel_offset = 3;
        fixture.fixture_type.parameters[0].kind = Cyan;
        assert!(fixture.color_channels(FixtureColorModel::Cmy).is_err());
        fixture.fixture_type.parameters[0].kind = Intensity;
        fixture.base_address = 511;
        assert!(fixture.color_channels(FixtureColorModel::Cmy).is_err());
        assert_eq!(fixture.resolve_channel(2), None, "a 16-bit channel cannot use slot 512 for coarse");
    }

    #[test]
    fn native_hsi_uses_only_explicit_color_intensity_and_preserves_the_master() {
        use ParamKind::{HsiIntensity, Hue, Intensity, Saturation};
        let mut fixture_type = builtin_fixture_types()
            .into_iter()
            .find(|fixture| fixture.name == "HSI (3ch)")
            .unwrap();
        fixture_type.parameters.insert(0, param(Intensity, "Master", 3, ChannelWidth::Bit8, 0.0));
        let mut fixture = PatchedFixture::new("Color wash", 5, 401, fixture_type);
        let channels = fixture.color_channels(FixtureColorModel::Hsi).unwrap();
        assert_eq!(channels.map(|slot| slot.channel), [400, 401, 402]);
        assert!(channels.iter().all(|slot| slot.universe == 5 && slot.width == ChannelWidth::Bit8));
        assert_eq!([Hue.identify_value(), Saturation.identify_value(), HsiIntensity.identify_value()], [0.0, 0.0, 1.0]);
        assert_eq!(serde_json::to_string(&HsiIntensity).unwrap(), "\"hsi_intensity\"");

        fixture.fixture_type.parameters[1].width = ChannelWidth::Bit16;
        assert!(fixture.color_channels(FixtureColorModel::Hsi).is_err());
        fixture.fixture_type.parameters[1].width = ChannelWidth::Bit8;
        fixture.fixture_type.parameters[3].kind = Intensity;
        assert!(fixture.color_channels(FixtureColorModel::Hsi).is_err());
        fixture.fixture_type.parameters[3].kind = HsiIntensity;
        fixture.fixture_type.parameters[0].kind = HsiIntensity;
        assert!(fixture.color_channels(FixtureColorModel::Hsi).is_err());
    }

    #[test]
    fn native_hsv_requires_explicit_value_and_valid_coarse_and_fine_slots() {
        use ParamKind::{HsiIntensity, HsvValue, Intensity};

        let fixture_type = builtin_fixture_types().into_iter()
            .find(|fixture| fixture.name == "HSV (3ch)").unwrap();
        let mut fixture = PatchedFixture::new("HSV wash", 7, 101, fixture_type);
        fixture.fixture_type.parameters[2].width = ChannelWidth::Bit16;
        fixture.fixture_type.parameters.push(param(Intensity, "Master dimmer", 4, ChannelWidth::Bit8, 0.0));
        let channels = fixture.color_channels(FixtureColorModel::Hsv).unwrap();
        assert_eq!(channels.map(|slot| slot.channel), [100, 101, 102]);
        assert_eq!(channels[2].slots().collect::<Vec<_>>(), [(7, 102), (7, 103)]);
        assert_eq!(HsvValue.identify_value(), 1.0);
        assert_eq!(serde_json::to_string(&HsvValue).unwrap(), "\"hsv_value\"");
        assert_eq!(serde_json::to_string(&FixtureColorModel::Hsv).unwrap(), "\"hsv\"");
        assert!(fixture.color_channels(FixtureColorModel::Hsi).is_err());

        fixture.fixture_type.parameters[3].kind = HsiIntensity;
        assert!(fixture.color_channels(FixtureColorModel::Hsv).is_err());
        assert!(fixture.color_channels(FixtureColorModel::Hsi).is_err());
        fixture.fixture_type.parameters[3].kind = Intensity;
        fixture.fixture_type.parameters[2].kind = Intensity;
        assert!(fixture.color_channels(FixtureColorModel::Hsv).is_err());
        fixture.fixture_type.parameters[2].kind = HsvValue;
        fixture.fixture_type.parameters[3].channel_offset = 3;
        assert!(fixture.color_channels(FixtureColorModel::Hsv).is_err());
    }

    #[test]
    fn group_color_requires_every_member_and_exclusive_physical_slots() {
        let fixture_type = builtin_fixture_types().into_iter()
            .find(|fixture| fixture.name == "CMY (3ch)").unwrap();
        let mut first = PatchedFixture::new("First", 1, 11, fixture_type.clone());
        first.fixture_type.parameters[0].width = ChannelWidth::Bit16;
        first.fixture_type.parameters[1].channel_offset = 2;
        first.fixture_type.parameters[2].channel_offset = 3;
        let second = PatchedFixture::new("Second", 2, 11, fixture_type);
        let group = FixtureGroup::new("Filters", vec![first.id, second.id]);
        let mut fixtures = vec![first, second];

        let channels = group.color_channels(&fixtures, FixtureColorModel::Cmy).unwrap();
        assert_eq!(channels[0].channels[0], PatchedChannel { universe: 1, channel: 10, width: ChannelWidth::Bit16 });
        assert_eq!(channels[1].channels[0], PatchedChannel { universe: 2, channel: 10, width: ChannelWidth::Bit8 });
        assert!(group.color_channels(&fixtures[..1], FixtureColorModel::Cmy).is_err());
        assert!(group.color_channels(&fixtures, FixtureColorModel::Hsi).is_err());
        let repeated = FixtureGroup::new("Duplicate", vec![fixtures[0].id, fixtures[0].id]);
        assert!(repeated.color_channels(&fixtures, FixtureColorModel::Cmy).is_err());

        let dimmer = builtin_fixture_types().remove(0);
        fixtures.push(PatchedFixture::new("Other fixture", 1, 12, dimmer));
        assert!(group.color_channels(&fixtures, FixtureColorModel::Cmy).unwrap_err().contains("Other fixture"));
        fixtures.pop();
        fixtures[1].fixture_type.parameters[0].kind = ParamKind::Hue;
        assert!(group.color_channels(&fixtures, FixtureColorModel::Cmy).is_err());
    }
}
