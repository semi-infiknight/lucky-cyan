//! Convert verified rows from QLab's nested LightPatch archive to an offline patch.
//! A USB port identifies a source universe, never a destination for live output.
use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use super::archive;
use crate::engine::{
    dmx_engine::ChannelWidth,
    fixture::{FixtureParam, FixtureType, ParamKind, PatchedFixture},
};

#[derive(Default)]
pub(super) struct Patch {
    pub fixtures: Vec<PatchedFixture>,
    pub notes: Vec<String>,
}

pub(super) fn from_settings(settings: &Value) -> Patch {
    let Some(blob) = settings.pointer("/Light/patch").filter(|v| !v.is_null()) else {
        return Patch::default();
    };
    let Some(patch) = archive::nested_data(blob).and_then(|b| archive::resolve(&b).ok()) else {
        return Patch {
            notes: vec![
                "The lighting patch could not be decoded. Rebuild it in DMX → Patch.".into(),
            ],
            ..Default::default()
        };
    };
    convert(&patch)
}

fn convert(patch: &Value) -> Patch {
    let mut result = Patch::default();
    let Some(items) = patch["itemList"]
        .as_array()
        .filter(|_| patch["__class__"] == "LightPatch")
    else {
        result
            .notes
            .push("Unrecognized lighting patch layout. Rebuild it in DMX → Patch.".into());
        return result;
    };
    let definitions = patch.pointer("/library/library/dictionary");
    let mut ports = HashMap::new();
    for item in items {
        let label = item["name"].as_str().unwrap_or("").trim();
        let convert_one = || -> Result<_, String> {
            if item["__class__"] != "LightCueInstrument" || label.is_empty() {
                return Err("unsupported patch entry or missing fixture name".into());
            }
            let name = item["definitionName"]
                .as_str()
                .ok_or("missing definition name")?;
            let profile = definitions
                .and_then(|defs| defs.get(name))
                .ok_or("missing instrument definition")?;
            let fixture_type = definition(profile)?;
            let start = item["address"]
                .as_u64()
                .filter(|n| *n < 512)
                .ok_or("invalid DMX start address")? as u16
                + 1;
            if start + fixture_type.footprint() - 1 > 512 {
                return Err("fixture extends beyond address 512".into());
            }
            let universe = &item["dmxUniverse"];
            let usb = &universe["backendInfo"];
            if universe["__class__"] != "LightDMXUniverse"
                || universe["backendType"] != 1
                || usb["__class__"] != "LightDMXUSBInfo"
            {
                return Err("unverified universe backend".into());
            }
            let serial = usb["deviceSerial"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("missing USB device identity")?;
            let port = usb["universe"]
                .as_u64()
                .filter(|n| {
                    usb["deviceNumUniverses"]
                        .as_u64()
                        .is_some_and(|count| *n < count)
                })
                .ok_or("invalid USB port")?;
            Ok((fixture_type, start, (serial.to_owned(), port)))
        };
        match convert_one() {
            Ok((profile, start, port)) => {
                let next = ports.len() + 1;
                let local = if let Some(&existing) = ports.get(&port) {
                    existing
                } else if next <= 63999 {
                    result.notes.push(format!("USB {} port {} → offline universe {next}. Assign the venue output after checking the patch.", port.0, port.1 + 1));
                    ports.insert(port, next as u16);
                    next as u16
                } else {
                    result
                        .notes
                        .push(format!("Rebuild {label}: no free logical universes."));
                    continue;
                };
                result
                    .fixtures
                    .push(PatchedFixture::new(label, local, start, profile));
            }
            Err(reason) => result.notes.push(format!(
                "Rebuild {}: {reason}.",
                if label.is_empty() {
                    "unnamed lighting item"
                } else {
                    label
                }
            )),
        }
    }
    if !result.fixtures.is_empty() {
        result.notes.insert(0, format!("Imported {} fixture(s) offline. Review roles, color mixing and addresses under DMX → Patch → Profile. Light cues require separate review.", result.fixtures.len()));
    }
    result
}

fn role(name: &str) -> ParamKind {
    match name.to_ascii_lowercase().as_str() {
        "intensity" => ParamKind::Intensity,
        "red" => ParamKind::Red,
        "green" => ParamKind::Green,
        "blue" => ParamKind::Blue,
        "white" => ParamKind::White,
        "amber" => ParamKind::Amber,
        "uv" => ParamKind::Uv,
        "pan" => ParamKind::Pan,
        "tilt" => ParamKind::Tilt,
        _ => ParamKind::Generic,
    }
}

fn definition(def: &Value) -> Result<FixtureType, String> {
    let name = def["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("missing model name")?;
    if def["__class__"] != "LightCueInstrumentDefinition"
        || !matches!(def["definitionVersion"].as_i64(), Some(1 | 2))
    {
        return Err(format!("unverified definition for {name}"));
    }
    let params = def["parameters"]
        .as_object()
        .ok_or("missing parameter table")?;
    let mut slots = BTreeMap::new();
    let mut colors = HashMap::new();
    let mut names = HashSet::new();
    for (key, p) in params {
        let offset = key
            .parse::<i32>()
            .map_err(|_| format!("invalid channel offset {key}"))?;
        if offset < 0 {
            if offset != -2
                || p["type"] != "rgbcolor"
                || p["__class__"] != "LightCueInstrumentDefinitionParameterRGBColor"
            {
                return Err(format!("unsupported virtual parameter {key}"));
            }
            for (color, target) in p["outputs"]
                .as_object()
                .ok_or("missing virtual color outputs")?
            {
                if !matches!(color.as_str(), "red" | "green" | "blue" | "white" | "amber") {
                    return Err(format!("unsupported color role {color}"));
                }
                let target = target.as_str().ok_or("unsupported color mapping")?;
                if colors.insert(target, role(color)).is_some() {
                    return Err(format!("multiple color roles for {target}"));
                }
            }
            continue;
        }
        if offset > 511
            || p["type"] != "scalar"
            || p["twoBytes"] != false
            || p["valueIsPercentage"] != true
            || p["homeValue"].as_u64() != Some(0)
            || p["homeValueDouble"].as_f64() != Some(0.0)
            || !matches!(
                p["__class__"].as_str(),
                Some(
                    "LightCueInstrumentDefinitionParameter"
                        | "LightCueInstrumentDefinitionParameterScalar"
                )
            )
            || p.as_object().is_none_or(|fields| {
                fields.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "__class__"
                            | "type"
                            | "name"
                            | "twoBytes"
                            | "valueIsPercentage"
                            | "homeValue"
                            | "homeValueDouble"
                    )
                })
            })
        {
            return Err(format!(
                "channel {} uses an unverified type, home level or modifier",
                offset.saturating_add(1)
            ));
        }
        let param = p["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("missing parameter name")?;
        if !names.insert(param) || slots.insert(offset as u16, param).is_some() {
            return Err(format!("duplicate channel or parameter {param}"));
        }
    }
    if slots.is_empty() || colors.keys().any(|target| !names.contains(target)) {
        return Err("missing scalar channel or unresolved color mapping".into());
    }
    let parameters = slots
        .into_iter()
        .map(|(channel_offset, name)| FixtureParam {
            kind: colors.get(name).copied().unwrap_or_else(|| {
                let inferred = role(name);
                if colors.values().any(|assigned| *assigned == inferred) {
                    ParamKind::Generic
                } else {
                    inferred
                }
            }),
            name: name.to_owned(),
            channel_offset,
            fine_channel_offset: None,
            width: ChannelWidth::Bit8,
            default: 0.0,
        })
        .collect();
    Ok(FixtureType {
        manufacturer: def["manufacturer"].as_str().unwrap_or("").to_owned(),
        name: name.to_owned(),
        mode: String::new(),
        parameters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patch() -> Value {
        json!({"__class__":"LightPatch","library":{"library":{"dictionary":{"RGB":{
            "__class__":"LightCueInstrumentDefinition","definitionVersion":2,"manufacturer":"Example","name":"RGB",
            "parameters":{"0":{"__class__":"LightCueInstrumentDefinitionParameterScalar","type":"scalar","name":"Cell A","valueIsPercentage":true,"twoBytes":false,"homeValue":0,"homeValueDouble":0.0},
            "-2":{"__class__":"LightCueInstrumentDefinitionParameterRGBColor","type":"rgbcolor","outputs":{"red":"Cell A"}}}}}}},
            "itemList":[{"__class__":"LightCueInstrument","name":"Wash","definitionName":"RGB","address":1,
            "dmxUniverse":{"__class__":"LightDMXUniverse","backendType":1,"backendInfo":{"__class__":"LightDMXUSBInfo","deviceSerial":"A","deviceNumUniverses":1,"universe":0}}}]})
    }

    #[test]
    fn maps_named_outputs_and_separates_distinct_source_devices() {
        let mut src = patch();
        let mut second = src["itemList"][0].clone();
        let third = second.clone();
        second["dmxUniverse"]["backendInfo"]["deviceSerial"] = json!("B");
        src["itemList"]
            .as_array_mut()
            .unwrap()
            .extend([second, third]);
        let p = convert(&src);
        assert_eq!(
            p.fixtures.iter().map(|f| f.universe).collect::<Vec<_>>(),
            [1, 2, 1]
        );
        assert_eq!(p.fixtures[0].base_address, 2);
        let param = &p.fixtures[0].fixture_type.parameters[0];
        assert_eq!(param.name, "Cell A");
        assert_eq!(param.kind, ParamKind::Red);
    }

    #[test]
    fn skips_whole_instruments_on_unrepresentable_channels_and_unverified_routes() {
        let source = patch();
        for (pointer, value) in [
            ("/itemList/0/address", json!(512)),
            ("/itemList/0/dmxUniverse/backendType", json!(9)),
            (
                "/library/library/dictionary/RGB/parameters/0/twoBytes",
                json!(true),
            ),
            (
                "/library/library/dictionary/RGB/parameters/0/curve",
                json!("custom"),
            ),
            (
                "/library/library/dictionary/RGB/parameters/-2/outputs/red",
                json!("absent"),
            ),
        ] {
            let mut sample = source.clone();
            if pointer.ends_with("/curve") {
                sample["library"]["library"]["dictionary"]["RGB"]["parameters"]["0"]["curve"] =
                    value;
            } else {
                *sample.pointer_mut(pointer).unwrap() = value;
            }
            let result = convert(&sample);
            assert!(result.fixtures.is_empty(), "{pointer}");
            assert!(result.notes.iter().any(|s| s.contains("Rebuild Wash")));
        }
    }

    #[test]
    #[ignore = "needs INKUE_QTABLETOP_FIXTURE pointing to the public QTableTop.qlab4"]
    fn archived_qtabletop_sixpar_maps_to_six_addressable_channels() {
        let path = std::env::var_os("INKUE_QTABLETOP_FIXTURE").unwrap();
        let archive = archive::resolve(&std::fs::read(path).unwrap()).unwrap();
        let patch = from_settings(&archive["settings"]);
        assert_eq!(patch.fixtures.len(), 1, "{:?}", patch.notes);
        let f = &patch.fixtures[0];
        assert_eq!(f.label, "SixPar");
        assert_eq!(f.fixture_type.manufacturer, "Elation");
        assert_eq!(f.fixture_type.name, "SIXPAR series - 6 channel");
        assert_eq!(f.address_span(), Some((1, 6)));
        for (i, role) in [
            ParamKind::Red,
            ParamKind::Green,
            ParamKind::Blue,
            ParamKind::White,
            ParamKind::Amber,
            ParamKind::Uv,
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(f.fixture_type.parameters[i].kind, *role);
            assert_eq!(
                f.resolve_channel(i),
                Some(crate::engine::dmx_engine::PatchedChannel::adjacent(1, i as u16, ChannelWidth::Bit8))
            );
        }
    }
}
