//! Global engine overrides (QLab's Override window equivalent). Each flag
//! disables one input or output path process-wide; all default to enabled.
//! Loopback/local traffic is never blocked by the network override.
use std::sync::atomic::{AtomicBool, Ordering};

/// QLab's full Override-window key set (13 paths).
pub const OVERRIDE_KEYS: [&str; 13] = [
    "dmx_output",
    "midi_input",
    "midi_output",
    "msc_input",
    "msc_output",
    "network_external_input",
    "network_external_output",
    "network_local_input",
    "network_local_output",
    "sysex_input",
    "sysex_output",
    "timecode_input",
    "timecode_output",
];

pub struct GlobalOverrides {
    dmx_output: AtomicBool,
    midi_input: AtomicBool,
    midi_output: AtomicBool,
    msc_input: AtomicBool,
    msc_output: AtomicBool,
    network_external_input: AtomicBool,
    network_external_output: AtomicBool,
    network_local_input: AtomicBool,
    network_local_output: AtomicBool,
    sysex_input: AtomicBool,
    sysex_output: AtomicBool,
    timecode_input: AtomicBool,
    timecode_output: AtomicBool,
    /// Visibility of the Override window (QLab `/overrideWindow`).
    window_open: AtomicBool,
}

pub static OVERRIDES: GlobalOverrides = GlobalOverrides::new();

impl GlobalOverrides {
    pub const fn new() -> Self {
        Self {
            dmx_output: AtomicBool::new(true),
            midi_input: AtomicBool::new(true),
            midi_output: AtomicBool::new(true),
            msc_input: AtomicBool::new(true),
            msc_output: AtomicBool::new(true),
            network_external_input: AtomicBool::new(true),
            network_external_output: AtomicBool::new(true),
            network_local_input: AtomicBool::new(true),
            network_local_output: AtomicBool::new(true),
            sysex_input: AtomicBool::new(true),
            sysex_output: AtomicBool::new(true),
            timecode_input: AtomicBool::new(true),
            timecode_output: AtomicBool::new(true),
            window_open: AtomicBool::new(false),
        }
    }

    pub fn get(&self, key: &str) -> Option<bool> {
        match key {
            "dmx_output" => Some(self.dmx_output()),
            "midi_input" => Some(self.midi_input()),
            "midi_output" => Some(self.midi_output()),
            "msc_input" => Some(self.msc_input()),
            "msc_output" => Some(self.msc_output()),
            "network_external_input" => Some(self.network_external_input()),
            "network_external_output" => Some(self.network_external_output()),
            "network_local_input" => Some(self.network_local_input()),
            "network_local_output" => Some(self.network_local_output()),
            "sysex_input" => Some(self.sysex_input()),
            "sysex_output" => Some(self.sysex_output()),
            "timecode_input" => Some(self.timecode_input()),
            "timecode_output" => Some(self.timecode_output()),
            _ => None,
        }
    }

    pub fn set(&self, key: &str, enabled: bool) -> Result<(), String> {
        let flag = match key {
            "dmx_output" => &self.dmx_output,
            "midi_input" => &self.midi_input,
            "midi_output" => &self.midi_output,
            "msc_input" => &self.msc_input,
            "msc_output" => &self.msc_output,
            "network_external_input" => &self.network_external_input,
            "network_external_output" => &self.network_external_output,
            "network_local_input" => &self.network_local_input,
            "network_local_output" => &self.network_local_output,
            "sysex_input" => &self.sysex_input,
            "sysex_output" => &self.sysex_output,
            "timecode_input" => &self.timecode_input,
            "timecode_output" => &self.timecode_output,
            _ => return Err(format!("Unknown override '{key}'.")),
        };
        flag.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    pub fn dmx_output(&self) -> bool { self.dmx_output.load(Ordering::Relaxed) }
    pub fn midi_input(&self) -> bool { self.midi_input.load(Ordering::Relaxed) }
    pub fn midi_output(&self) -> bool { self.midi_output.load(Ordering::Relaxed) }
    pub fn msc_input(&self) -> bool { self.msc_input.load(Ordering::Relaxed) }
    pub fn msc_output(&self) -> bool { self.msc_output.load(Ordering::Relaxed) }
    pub fn network_external_input(&self) -> bool { self.network_external_input.load(Ordering::Relaxed) }
    pub fn network_external_output(&self) -> bool { self.network_external_output.load(Ordering::Relaxed) }
    pub fn network_local_input(&self) -> bool { self.network_local_input.load(Ordering::Relaxed) }
    pub fn network_local_output(&self) -> bool { self.network_local_output.load(Ordering::Relaxed) }
    pub fn sysex_input(&self) -> bool { self.sysex_input.load(Ordering::Relaxed) }
    pub fn sysex_output(&self) -> bool { self.sysex_output.load(Ordering::Relaxed) }
    pub fn timecode_input(&self) -> bool { self.timecode_input.load(Ordering::Relaxed) }
    pub fn timecode_output(&self) -> bool { self.timecode_output.load(Ordering::Relaxed) }

    pub fn override_window(&self) -> bool { self.window_open.load(Ordering::Relaxed) }
    pub fn set_override_window(&self, open: bool) { self.window_open.store(open, Ordering::Relaxed); }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "dmx_output": self.dmx_output(),
            "midi_input": self.midi_input(),
            "midi_output": self.midi_output(),
            "msc_input": self.msc_input(),
            "msc_output": self.msc_output(),
            "network_external_input": self.network_external_input(),
            "network_external_output": self.network_external_output(),
            "network_local_input": self.network_local_input(),
            "network_local_output": self.network_local_output(),
            "sysex_input": self.sysex_input(),
            "sysex_output": self.sysex_output(),
            "timecode_input": self.timecode_input(),
            "timecode_output": self.timecode_output(),
        })
    }
}

impl Default for GlobalOverrides {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_round_trips_and_unknown_keys_are_rejected() {
        // Use a local instance so parallel tests never race on the global static.
        let local = GlobalOverrides::new();
        for key in OVERRIDE_KEYS {
            assert_eq!(local.get(key), Some(true));
            local.set(key, false).unwrap();
            assert_eq!(local.get(key), Some(false));
            local.set(key, true).unwrap();
            assert_eq!(local.get(key), Some(true));
        }
        assert!(local.get("nope").is_none());
        assert!(local.set("nope", true).is_err());
        let snap = local.snapshot();
        for key in OVERRIDE_KEYS {
            assert_eq!(snap[key], serde_json::Value::Bool(true), "{key}");
        }
        // The global static starts fully enabled.
        for key in OVERRIDE_KEYS {
            assert_eq!(OVERRIDES.get(key), Some(true), "{key}");
        }
    }
}
