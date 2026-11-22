//! Persistence for machine-level configs in `%APPDATA%\Inkue\`.
//!
//! These files are intentionally separate from the workspace (`.inkue`)
//! because hardware settings are machine-specific — the workspace travels
//! with the show while these stay on the machine.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::{
    preferences::{MachineAudioConfig, NetworkInterfaceConfig, OscReceiveConfig},
    engine::timecode_receiver::TcReceiverConfig,
};

/// Per-OS base directory for machine-level config files.
///
/// Falls back to the current directory only if the platform's expected
/// environment variable is unset — this must never resolve into the source
/// tree (`src-tauri/`), or writes during `tauri dev` retrigger its file
/// watcher and restart the whole app.
pub(crate) fn config_base_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support"))
            .unwrap_or_else(|_| PathBuf::from("."))
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|_| PathBuf::from("."))
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct WorkspaceTemplateConfig {
    default_template: Option<PathBuf>,
}

fn workspace_templates_path() -> PathBuf {
    config_base_dir().join("Inkue").join("workspace-templates.json")
}

/// The file to use for ordinary New Workspace on this machine.
pub(crate) fn load_default_workspace_template() -> anyhow::Result<Option<PathBuf>> {
    read_workspace_template_config(&workspace_templates_path())
}

fn read_workspace_template_config(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let json = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Could not read the default workspace template setting"),
    };
    let settings: WorkspaceTemplateConfig = serde_json::from_str(&json)
        .context("The default workspace template setting could not be read")?;
    Ok(settings.default_template)
}

/// Persist a reference without copying, moving or deleting the template file.
pub(crate) fn save_default_workspace_template(default_template: Option<PathBuf>) -> anyhow::Result<()> {
    write_workspace_template_config(&workspace_templates_path(), default_template)
}

fn write_workspace_template_config(path: &Path, default_template: Option<PathBuf>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("Could not create the Inkue settings folder")?;
    }
    let bytes = serde_json::to_vec_pretty(&WorkspaceTemplateConfig { default_template })?;
    crate::recovery::write_document(path, &bytes).context("Could not save the default workspace template setting")
}

#[cfg(test)]
mod workspace_template_config_tests {
    use super::*;

    #[test]
    fn selecting_and_clearing_the_default_changes_only_the_local_reference() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("Inkue").join("workspace-templates.json");
        let starter = directory.path().join("House start.inkue-template");
        std::fs::write(&starter, b"template contents").unwrap();

        assert_eq!(read_workspace_template_config(&config).unwrap(), None);
        write_workspace_template_config(&config, Some(starter.clone())).unwrap();
        assert_eq!(read_workspace_template_config(&config).unwrap(), Some(starter.clone()));
        assert_eq!(std::fs::read(&starter).unwrap(), b"template contents");

        std::fs::write(&config, b"invalid").unwrap();
        assert!(read_workspace_template_config(&config).is_err());
        write_workspace_template_config(&config, None).unwrap();
        assert_eq!(read_workspace_template_config(&config).unwrap(), None);
        assert_eq!(std::fs::read(&starter).unwrap(), b"template contents");
    }
}

/// Absolute path to the machine audio config file.
fn config_path() -> PathBuf {
    config_base_dir().join("Inkue").join("audio.json")
}

/// Load the machine audio config from disk.  Returns [`MachineAudioConfig::default`]
/// on first run or if the file cannot be read/parsed.
pub fn load() -> MachineAudioConfig {
    let path = config_path();
    let mut config: MachineAudioConfig = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // Normalise here, not in the Preferences command: the engine reads this
    // same function at startup, and a backend from another OS would otherwise
    // reach the audio log and the stream builder unchanged.
    config.backend = config.backend.for_this_platform();
    config
}

/// Persist the machine audio config to disk, creating `%APPDATA%\Inkue\` if needed.
pub fn save(config: &MachineAudioConfig) -> anyhow::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, json)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// OSC receive config
// ---------------------------------------------------------------------------

fn osc_config_path() -> PathBuf {
    config_base_dir().join("Inkue").join("osc.json")
}

/// Load the OSC receive config from disk.  Returns the default config on first
/// run or when the file cannot be read/parsed.
pub fn load_osc() -> OscReceiveConfig {
    let path = osc_config_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the OSC receive config to disk, creating `%APPDATA%\Inkue\` if needed.
pub fn save_osc(config: &OscReceiveConfig) -> anyhow::Result<()> {
    let path = osc_config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, json)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Network interface config
// ---------------------------------------------------------------------------

fn network_config_path() -> PathBuf {
    config_base_dir().join("Inkue").join("network.json")
}

/// Load the network interface config from disk.  Returns the default
/// (Automatic — all interfaces) on first run or when the file cannot be
/// read/parsed.
pub fn load_network() -> NetworkInterfaceConfig {
    let path = network_config_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the network interface config to disk.
pub fn save_network(config: &NetworkInterfaceConfig) -> anyhow::Result<()> {
    let path = network_config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, json)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// TC machine config
// ---------------------------------------------------------------------------

/// Persisted TC machine config (receiver enabled/disabled + source/port).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[derive(Default)]
pub struct TcMachineConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub receiver_config: TcReceiverConfig,
}


// ---------------------------------------------------------------------------
// MIDI trigger machine config
// ---------------------------------------------------------------------------

/// Which MIDI input drives per-cue triggers on **this machine**.
///
/// Machine config, not workspace config: the port name belongs to the hardware
/// in front of the operator, so a show file carried to another rig keeps its
/// triggers but picks up that rig's input.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MidiTriggerMachineConfig {
    #[serde(default)]
    pub enabled: bool,
    /// MIDI input port name. `None` = the first available port.
    #[serde(default)]
    pub port: Option<String>,
}

fn midi_trigger_config_path() -> std::path::PathBuf {
    config_base_dir().join("Inkue").join("midi_triggers.json")
}

pub fn load_midi_trigger_config() -> MidiTriggerMachineConfig {
    std::fs::read_to_string(midi_trigger_config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_midi_trigger_config(config: &MidiTriggerMachineConfig) -> anyhow::Result<()> {
    let path = midi_trigger_config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(config)?)?;
    Ok(())
}

fn tc_config_path() -> std::path::PathBuf {
    config_base_dir().join("Inkue").join("timecode.json")
}

pub fn load_tc_config() -> TcMachineConfig {
    let path = tc_config_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_tc_config(config: &TcMachineConfig) -> anyhow::Result<()> {
    let path = tc_config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, json)?;
    Ok(())
}
