//! [`MidiCue`] — sends one or more MIDI messages when triggered.
//!
//! Single messages dispatch synchronously. Continuous voice messages can also
//! drive a timed fade with an explicitly programmed start, shape and end.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{anyhow, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    context::{CueContext, CueEvent},
    curve::{CurveShape, PreparedCurve},
    traits::{Cue, CueFactory},
    types::{ContinueMode, CueColor, CueId, CueState, CueType},
};

// ---------------------------------------------------------------------------
// MIDI message types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MidiMessageType {
    #[default]
    NoteOn,
    NoteOff,
    KeyPressure,
    ChannelPressure,
    PitchBend,
    ControlChange,
    ProgramChange,
    Msc,
    Sysex,
}

/// The maximum supported duration of one MIDI fade, in milliseconds.
pub const MAX_MIDI_FADE_MS: u64 = 86_400_000;
const MAX_MIDI_CURVE_POINTS: usize = 64;
const MAX_SYSEX_MESSAGE_BYTES: usize = 4096;
const MAX_SYSEX_TEXT_BYTES: usize = 16_384;

fn sysex_bytes(text: &str) -> Result<Vec<u8>> {
    ensure!(text.len() <= MAX_SYSEX_TEXT_BYTES, "SysEx text exceeds {MAX_SYSEX_TEXT_BYTES} characters");
    let mut bytes = Vec::new();
    for word in text.split_ascii_whitespace() {
        ensure!(word.len() % 2 == 0, "Each SysEx byte needs two hexadecimal digits");
        for digits in word.as_bytes().chunks_exact(2) {
            let high = char::from(digits[0]).to_digit(16);
            let low = char::from(digits[1]).to_digit(16);
            let (Some(high), Some(low)) = (high, low) else {
                return Err(anyhow!("SysEx input must contain hexadecimal bytes separated by whitespace"));
            };
            ensure!(bytes.len() < MAX_SYSEX_MESSAGE_BYTES, "SysEx messages can contain up to {MAX_SYSEX_MESSAGE_BYTES} bytes including F0 and F7");
            bytes.push((high * 16 + low) as u8);
        }
    }

    let has_start = bytes.first() == Some(&0xF0);
    let has_end = bytes.last() == Some(&0xF7);
    ensure!(has_start == has_end, "Supply both F0 and F7 or omit both");
    let data = if has_start { &bytes[1..bytes.len() - 1] } else { &bytes };
    ensure!(!data.is_empty(), "Enter at least one SysEx data byte");
    ensure!(data.len() + 2 <= MAX_SYSEX_MESSAGE_BYTES, "SysEx messages can contain up to {MAX_SYSEX_MESSAGE_BYTES} bytes including F0 and F7");
    ensure!(data.iter().all(|byte| *byte < 0x80), "SysEx data bytes must be 00–7F");
    ensure!(data[0] != 0 || data.len() >= 3, "A SysEx manufacturer ID starting with 00 needs three bytes");

    if has_start {
        Ok(bytes)
    } else {
        Ok(std::iter::once(0xF0).chain(bytes).chain(std::iter::once(0xF7)).collect())
    }
}

/// Envelope and destination of one continuous MIDI voice value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiFade {
    /// Fade duration in milliseconds.
    pub duration_ms: u64,
    /// Ending value, 0–127 for controllers and pressure, −8192–8191 for bend.
    pub end_value: i16,
    /// Older rows without a shape interpolate linearly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<CurveShape>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiMessage {
    /// Name of the MIDI output port to send on.
    pub port_name: String,
    pub message_type: MidiMessageType,
    /// MIDI channel, 1–16.
    pub channel: u8,
    /// First 7-bit MIDI data byte (least significant bits for pitch bend).
    pub data1: u8,
    /// Second 7-bit MIDI data byte. Ignored for program and channel pressure.
    pub data2: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msc: Option<super::msc::MscMessage>,
    /// Hexadecimal SysEx data with optional F0/F7 framing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sysex: Option<String>,
    /// If configured, GO sends the start value and interpolates to this end value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade: Option<MidiFade>,
}

impl MidiMessage {
    /// Encode the single message at its programmed start value.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let ch = self.channel.saturating_sub(1) & 0x0F;
        let d1 = self.data1 & 0x7F;
        let d2 = self.data2 & 0x7F;
        Ok(match self.message_type {
            // Broadcast form: MSC messages may use the all-call device ID 127
            // (cue destinations are constrained to 0..=111 at import/UI time).
            MidiMessageType::Msc => self.msc.as_ref().ok_or_else(|| anyhow::anyhow!("Missing MSC settings"))?
                .to_broadcast_bytes().map_err(anyhow::Error::msg)?,
            MidiMessageType::Sysex => sysex_bytes(self.sysex.as_deref().unwrap_or_default())?,
            MidiMessageType::NoteOff       => vec![0x80 | ch, d1, d2],
            MidiMessageType::NoteOn        => vec![0x90 | ch, d1, d2],
            MidiMessageType::KeyPressure   => vec![0xA0 | ch, d1, d2],
            MidiMessageType::ChannelPressure => vec![0xD0 | ch, d1],
            MidiMessageType::PitchBend     => vec![0xE0 | ch, d1, d2],
            MidiMessageType::ControlChange => vec![0xB0 | ch, d1, d2],
            MidiMessageType::ProgramChange => vec![0xC0 | ch, d1],
        })
    }

    fn needs_prepared_output(&self) -> bool {
        self.fade.is_some() || self.message_type == MidiMessageType::Sysex
    }

    fn ramp(&self, fade: &MidiFade) -> Result<MidiRamp> {
        ensure!(!self.port_name.trim().is_empty(), "Select an output port for the MIDI fade");
        ensure!((1..=16).contains(&self.channel), "The MIDI fade channel must be 1–16");
        ensure!(self.data1 <= 127 && self.data2 <= 127, "MIDI fade start bytes must be 0–127");
        ensure!((1..=MAX_MIDI_FADE_MS).contains(&fade.duration_ms), "MIDI fade duration must be 1 ms to 24 hours");
        if let Some(curve) = &fade.curve {
            ensure!(curve.points.len() <= MAX_MIDI_CURVE_POINTS, "A MIDI fade curve can have up to {MAX_MIDI_CURVE_POINTS} control points");
            ensure!(curve.bends.len() <= curve.points.len() + 1, "A MIDI fade curve needs at most one bend per segment");
            ensure!((-10.0..=10.0).contains(&curve.intensity)
                && curve.points.iter().all(|point| (0.0..=1.0).contains(&point.t) && (0.0..=1.0).contains(&point.v))
                && curve.bends.iter().all(|bend| (-1.0..=1.0).contains(bend)), "MIDI fade curve values are outside the supported range");
        }
        let channel = self.channel - 1;
        let (status, field, wire_start, wire_end) = match self.message_type {
            MidiMessageType::ControlChange => {
                ensure!(self.data1 <= 119, "MIDI Channel Mode controllers 120–127 cannot be faded");
                (0xB0, FadeField::Second(self.data1), i16::from(self.data2), fade.end_value)
            }
            MidiMessageType::KeyPressure => (0xA0, FadeField::Second(self.data1), i16::from(self.data2), fade.end_value),
            MidiMessageType::ChannelPressure => (0xD0, FadeField::First, i16::from(self.data1), fade.end_value),
            MidiMessageType::PitchBend => {
                ensure!((-8192..=8191).contains(&fade.end_value), "MIDI fade end bend must be −8192–8191");
                (0xE0, FadeField::Pitch, i16::from(self.data2) * 128 + i16::from(self.data1), fade.end_value + 8192)
            }
            _ => return Err(anyhow!("This MIDI message type cannot be faded")),
        };
        ensure!(matches!(field, FadeField::Pitch) || (0..=127).contains(&wire_end), "MIDI fade end value must be 0–127");
        Ok(MidiRamp {
            status: status | channel,
            field,
            start: wire_start,
            end: wire_end,
            duration: Duration::from_millis(fade.duration_ms),
            curve: fade.curve.as_ref().map(CurveShape::prepare),
            last_value: None,
        })
    }
}

#[derive(Clone, Copy)]
enum FadeField {
    First,
    Second(u8),
    Pitch,
}

struct MidiRamp {
    status: u8,
    field: FadeField,
    start: i16,
    end: i16,
    duration: Duration,
    curve: Option<PreparedCurve>,
    last_value: Option<i16>,
}

impl MidiRamp {
    fn sample(&mut self, elapsed: Duration) -> Option<([u8; 3], usize)> {
        let value = if elapsed.is_zero() {
            self.start
        } else if elapsed >= self.duration {
            self.end
        } else if self.start == self.end {
            return None;
        } else {
            let time = elapsed.as_secs_f64() / self.duration.as_secs_f64();
            let progress = self.curve.as_ref().map_or(time, |curve| curve.sample(time));
            let value = (f64::from(self.start) + f64::from(self.end - self.start) * progress).round() as i16;
            // Reserve the precise end value for its deadline, even when a 7-bit
            // controller would round to it one tick too early.
            if self.start < self.end { value.min(self.end - 1) } else { value.max(self.end + 1) }
        };
        if self.last_value == Some(value) {
            return None;
        }
        self.last_value = Some(value);
        Some(match self.field {
            FadeField::First => ([self.status, value as u8, 0], 2),
            FadeField::Second(first) => ([self.status, first, value as u8], 3),
            FadeField::Pitch => ([self.status, (value & 127) as u8, (value >> 7) as u8], 3),
        })
    }
}

// ---------------------------------------------------------------------------
// MIDI send helper (pub so midi_cmds can reuse it)
// ---------------------------------------------------------------------------

/// Open each required MIDI port, send the message, and close.
/// Port open/close is negligible latency for fire-and-forget MIDI events.
pub fn send_midi_messages(messages: &[MidiMessage]) -> Result<()> {
    let mut errors = Vec::new();
    if messages.is_empty() || !crate::engine::overrides::OVERRIDES.midi_output() {
        return Ok(());
    }
    let encoded = messages.iter().map(MidiMessage::to_bytes).collect::<Result<Vec<_>>>()?;
    let mut midi_out = match midir::MidiOutput::new("Inkue") {
        Ok(m) => m,
        Err(e) => {
            log::warn!("MIDI: failed to create output: {e}");
            return Err(anyhow::anyhow!("MIDI output unavailable: {e}"));
        }
    };
    for (msg, bytes) in messages.iter().zip(encoded) {
        // QLab's output overrides gate by message kind, not by wire bytes:
        // sysexOutput drops SysEx cues, mscOutput drops MSC messages (both
        // are 0xF0-framed on the wire).
        let blocked = match msg.message_type {
            MidiMessageType::Sysex => !crate::engine::overrides::OVERRIDES.sysex_output(),
            MidiMessageType::Msc => !crate::engine::overrides::OVERRIDES.msc_output(),
            _ => false,
        };
        if blocked {
            log::debug!("MIDI: dropping {:?} message while its output override is off", msg.message_type);
            continue;
        }
        let ports = midi_out.ports();
        let port = ports
            .into_iter()
            .find(|p| midi_out.port_name(p).ok().as_deref() == Some(msg.port_name.as_str()));

        let alert_key = format!("midi:{}", msg.port_name);

        let Some(port) = port else {
            log::warn!("MIDI: port '{}' not found", msg.port_name);
            crate::health::set(crate::health::HealthAlert::new(
                &alert_key,
                crate::health::HealthLevel::Error,
                format!("MIDI port \"{}\" not found", msg.port_name),
            ));
            errors.push(format!("MIDI port {} not found", msg.port_name));
            continue;
        };

        match midi_out.connect(&port, "inkue") {
            Ok(mut conn) => {
                if let Err(e) = conn.send(&bytes) {
                    errors.push(format!("MIDI send to {} failed: {e}", msg.port_name));
                    log::warn!("MIDI send failed on '{}': {e}", msg.port_name);
                    crate::health::set(crate::health::HealthAlert::new(
                        &alert_key,
                        crate::health::HealthLevel::Error,
                        format!("MIDI send failed on \"{}\"", msg.port_name),
                    ));
                } else {
                    // The port is reachable again — drop any stale alert for it.
                    crate::health::clear(&alert_key);
                }
                midi_out = conn.close();
            }
            Err(e) => {
                errors.push(format!("Cannot connect to MIDI port {}", msg.port_name));
                log::warn!("MIDI: failed to connect to '{}': {:?}", msg.port_name, e.kind());
                crate::health::set(crate::health::HealthAlert::new(
                    &alert_key,
                    crate::health::HealthLevel::Error,
                    format!("Cannot connect to MIDI port \"{}\"", msg.port_name),
                ));
                midi_out = e.into_inner();
            }
        }
    }
    anyhow::ensure!(errors.is_empty(), "{}", errors.join(", "));
    Ok(())
}

type MidiSink = Box<dyn FnMut(&[u8]) -> Result<()> + Send>;

fn open_named_midi_port(name: &str) -> Result<MidiSink> {
    ensure!(!name.trim().is_empty(), "Select a MIDI output port");
    let output = midir::MidiOutput::new("Inkue")
        .map_err(|error| anyhow!("MIDI output unavailable: {error}"))?;
    let mut matches = output.ports().into_iter()
        .filter(|p| output.port_name(p).ok().as_deref() == Some(name));
    let port = matches.next().ok_or_else(|| anyhow!("MIDI port \"{name}\" not found"))?;
    ensure!(matches.next().is_none(), "More than one MIDI output is named \"{name}\"");
    let mut connection = output.connect(&port, "inkue-fade")
        .map_err(|error| anyhow!("Cannot connect to MIDI port \"{name}\": {:?}", error.kind()))?;
    Ok(Box::new(move |bytes| connection.send(bytes).map_err(anyhow::Error::from)))
}

/// Test-send to exactly one explicitly named port.
pub fn send_midi_preview(message: &MidiMessage) -> Result<()> {
    let bytes = message.to_bytes()?;
    let result = open_named_midi_port(&message.port_name).and_then(|mut sink| sink(&bytes));
    if let Err(error) = &result {
        set_midi_port_error(&message.port_name, error);
    } else {
        crate::health::clear(&format!("midi:{}", message.port_name));
    }
    result
}

fn set_midi_port_error(port: &str, error: &anyhow::Error) {
    log::warn!("MIDI cue on '{port}': {error}");
    crate::health::set(crate::health::HealthAlert::new(
        format!("midi:{port}"),
        crate::health::HealthLevel::Error,
        error.to_string(),
    ));
}

struct MidiPort {
    name: String,
    sink: Option<MidiSink>,
    sent: bool,
}

impl MidiPort {
    fn send(&mut self, bytes: &[u8]) {
        let Some(sink) = self.sink.as_mut() else { return };
        if let Err(error) = sink(bytes) {
            set_midi_port_error(&self.name, &anyhow!("MIDI send to \"{}\" failed: {error}", self.name));
            self.sink = None;
        } else if !self.sent {
            crate::health::clear(&format!("midi:{}", self.name));
            self.sent = true;
        }
    }
}

enum MidiRow {
    OneShot(Vec<u8>),
    Ramp(MidiRamp),
}

struct MidiRun {
    pre_wait: Duration,
    duration: Duration,
    active_since: Option<Instant>,
    elapsed_before_pause: Duration,
    action_offset: Option<Duration>,
    rows: Vec<(usize, MidiRow)>,
    ports: Vec<MidiPort>,
    finished: bool,
}

impl MidiRun {
    fn prepare(
        messages: &[MidiMessage],
        pre_wait: Duration,
        mut open: impl FnMut(&str) -> Result<MidiSink>,
    ) -> Result<Self> {
        let mut duration = Duration::ZERO;
        let mut names = Vec::new();
        let mut indices = HashMap::new();
        let mut rows = Vec::with_capacity(messages.len());
        for message in messages {
            let name = message.port_name.as_str();
            ensure!(!name.trim().is_empty(), "Select an output port for every MIDI message");
            let index = *indices.entry(name).or_insert_with(|| {
                names.push(name);
                names.len() - 1
            });
            let row = if let Some(fade) = &message.fade {
                let ramp = message.ramp(fade)?;
                duration = duration.max(ramp.duration);
                MidiRow::Ramp(ramp)
            } else {
                MidiRow::OneShot(message.to_bytes()?)
            };
            rows.push((index, row));
        }
        // Timed and SysEx cues open all explicit ports before sending any byte. An
        // invalid or ambiguous route cannot partially fire the other rows.
        let mut ports = Vec::with_capacity(names.len());
        for name in names {
            let sink = open(name).inspect_err(|error| set_midi_port_error(name, error))?;
            ports.push(MidiPort { name: name.into(), sink: Some(sink), sent: false });
        }
        Ok(Self {
            pre_wait, duration, rows, ports, finished: false,
            active_since: None, elapsed_before_pause: Duration::ZERO, action_offset: None,
        })
    }

    fn elapsed(&self, now: Instant) -> Duration {
        self.elapsed_before_pause + self.active_since.map_or(Duration::ZERO, |t| now.saturating_duration_since(t))
    }

    fn action_elapsed(&self, now: Instant) -> Duration {
        self.action_offset.map_or(Duration::ZERO, |offset| self.elapsed(now).saturating_sub(offset))
    }

    fn dispatch(&mut self, elapsed: Duration, starting: bool) {
        if self.finished {
            return;
        }
        for (index, row) in &mut self.rows {
            let port = &mut self.ports[*index];
            match row {
                MidiRow::OneShot(bytes) if starting => port.send(bytes),
                MidiRow::OneShot(_) => {},
                MidiRow::Ramp(ramp) => {
                    if let Some((bytes, len)) = ramp.sample(elapsed) {
                        port.send(&bytes[..len]);
                    }
                }
            }
        }
        if elapsed >= self.duration {
            self.finished = true;
            self.ports.clear();
        }
    }
}

// ---------------------------------------------------------------------------
// MidiCue
// ---------------------------------------------------------------------------

pub struct MidiCue {
    id: CueId,
    name: String,
    number: Option<String>,
    notes: String,
    color: CueColor,
    state: CueState,
    continue_mode: ContinueMode,
    pre_wait: Duration,
    post_wait: Duration,
    started_at: Option<Instant>,
    auto_continue_fired: bool,
    run: Option<MidiRun>,
    is_disabled: bool,
    /// The MIDI messages to send on GO.
    pub messages: Vec<MidiMessage>,
}

impl MidiCue {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::from("MIDI"),
            number: None,
            notes: String::new(),
            color: CueColor::White,
            state: CueState::Standby,
            continue_mode: ContinueMode::DoNotContinue,
            pre_wait: Duration::ZERO,
            post_wait: Duration::ZERO,
            started_at: None,
            auto_continue_fired: false,
            run: None,
            is_disabled: false,
            messages: Vec::new(),
        }
    }

    fn go_with(
        &mut self,
        now: impl FnOnce() -> Instant,
        open: impl FnMut(&str) -> Result<MidiSink>,
        mut emit: impl FnMut(CueEvent),
    ) -> Result<()> {
        if self.run.is_some() && self.state == CueState::Running {
            return Ok(());
        }
        let prepared = self.messages.iter().any(MidiMessage::needs_prepared_output);
        self.run = if prepared { Some(MidiRun::prepare(&self.messages, self.pre_wait, open)?) } else { None };
        let now = now();
        self.auto_continue_fired = false;
        self.state = CueState::Running;
        self.started_at = Some(now);
        if let Some(run) = self.run.as_mut() {
            run.active_since = Some(now);
            self.tick_at(now, emit);
            return Ok(());
        }

        emit(CueEvent::ActionStarted { cue_id: self.id });
        // Preserve the single-shot path and its per-port error reporting.
        if let Err(error) = send_midi_messages(&self.messages) {
            log::warn!("MIDI cue failed: {error}");
        }
        self.state = CueState::Completed;
        emit(CueEvent::ActionCompleted { cue_id: self.id });
        Ok(())
    }

    fn tick_at(&mut self, now: Instant, mut emit: impl FnMut(CueEvent)) {
        if self.state != CueState::Running {
            return;
        }
        let Some(run) = self.run.as_mut() else { return };
        if run.finished {
            return;
        }
        if run.action_offset.is_none() {
            let elapsed = run.elapsed(now);
            if elapsed < run.pre_wait {
                return;
            }
            run.action_offset = Some(elapsed);
            emit(CueEvent::ActionStarted { cue_id: self.id });
            run.dispatch(Duration::ZERO, true);
        } else {
            run.dispatch(run.action_elapsed(now), false);
        }
        if run.finished {
            emit(CueEvent::ActionCompleted { cue_id: self.id });
        }
    }

    fn pause_at(&mut self, now: Instant) {
        if self.state != CueState::Running {
            return;
        }
        let Some(run) = self.run.as_mut() else { return };
        run.elapsed_before_pause = run.elapsed(now);
        run.active_since = None;
        self.state = CueState::Paused;
    }

    fn resume_at(&mut self, now: Instant) {
        if self.state != CueState::Paused {
            return;
        }
        let Some(run) = self.run.as_mut() else { return };
        run.active_since = Some(now);
        self.state = CueState::Running;
    }
}

impl Default for MidiCue {
    fn default() -> Self {
        Self::new()
    }
}

impl Cue for MidiCue {
    inherit_playback_fields!(state, started_at, auto_continue_fired, run);

    fn id(&self) -> CueId { self.id }
    fn cue_type(&self) -> CueType { CueType::Midi }
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn number(&self) -> Option<&str> { self.number.as_deref() }
    fn set_number(&mut self, number: Option<String>) { self.number = number; }
    fn notes(&self) -> &str { &self.notes }
    fn set_notes(&mut self, notes: String) { self.notes = notes; }
    fn color(&self) -> CueColor { self.color }
    fn set_color(&mut self, color: CueColor) { self.color = color; }
    fn is_disabled(&self) -> bool { self.is_disabled }
    fn set_disabled(&mut self, d: bool) { self.is_disabled = d; }
    fn state(&self) -> CueState { self.state }

    fn load(&mut self, _context: &CueContext) -> Result<()> { Ok(()) }

    fn go(&mut self, context: &CueContext) -> Result<()> {
        self.go_with(Instant::now, open_named_midi_port, |event| context.emit_for_action(event))
    }

    fn stop(&mut self, context: &CueContext) -> Result<()> {
        self.reset()?;
        context.emit(CueEvent::Stopped { cue_id: self.id });
        Ok(())
    }

    fn pause(&mut self, _context: &CueContext) -> Result<()> {
        self.pause_at(Instant::now());
        Ok(())
    }

    fn resume(&mut self, _context: &CueContext) -> Result<()> {
        self.resume_at(Instant::now());
        Ok(())
    }

    fn hard_stop(&mut self, context: &CueContext) -> Result<()> {
        self.stop(context)
    }

    fn reset(&mut self) -> Result<()> {
        self.state = CueState::Standby;
        self.started_at = None;
        self.auto_continue_fired = false;
        self.run = None;
        Ok(())
    }

    fn tick(&mut self, context: &CueContext) -> Result<()> {
        self.tick_at(Instant::now(), |event| context.emit_for_action(event));
        Ok(())
    }

    fn is_action_started(&self) -> bool {
        self.run.as_ref().is_none_or(|run| run.action_offset.is_some())
    }

    fn uses_action_clock_for_continue(&self) -> bool {
        self.run.is_some()
    }

    fn pre_wait(&self) -> Duration { self.pre_wait }
    fn set_pre_wait(&mut self, d: Duration) { self.pre_wait = d; }
    fn post_wait(&self) -> Duration { self.post_wait }
    fn set_post_wait(&mut self, d: Duration) { self.post_wait = d; }

    fn duration(&self) -> Option<Duration> {
        if let Some(run) = &self.run {
            return Some(run.duration);
        }
        self.messages.iter().filter_map(|message| {
            let fade = message.fade.as_ref()?;
            (1..=MAX_MIDI_FADE_MS).contains(&fade.duration_ms).then(|| Duration::from_millis(fade.duration_ms))
        }).max()
    }

    fn elapsed(&self) -> Duration {
        self.run.as_ref().map_or_else(
            || self.started_at.map(|t| t.elapsed()).unwrap_or(Duration::ZERO),
            |run| run.elapsed(Instant::now()),
        )
    }

    fn action_elapsed(&self) -> Duration {
        let Some(run) = &self.run else { return self.elapsed() };
        let elapsed = run.action_elapsed(Instant::now());
        if run.finished { elapsed } else {
            // Completion is sampled on the same frame as the final write. A
            // deadline that passes just after Tick cannot reap the cue early.
            elapsed.min(run.duration.saturating_sub(Duration::from_nanos(1)))
        }
    }

    fn continue_mode(&self) -> ContinueMode { self.continue_mode }
    fn set_continue_mode(&mut self, mode: ContinueMode) { self.continue_mode = mode; }
    fn is_auto_continue_fired(&self) -> bool { self.auto_continue_fired }
    fn mark_auto_continue_fired(&mut self) { self.auto_continue_fired = true; }
    fn clear_auto_continue_fired(&mut self) { self.auto_continue_fired = false; }

    fn validate_programming(
        &self,
        ctx: &crate::cue::validation::ValidationContext,
    ) -> Vec<crate::cue::validation::CueIssue> {
        use crate::cue::validation::CueIssue;
        let mut issues = Vec::new();
        if self.messages.is_empty() {
            issues.push(CueIssue::warning("No MIDI messages"));
        }
        let prepared = self.messages.iter().any(MidiMessage::needs_prepared_output);
        for msg in &self.messages {
            let validation = if let Some(fade) = &msg.fade {
                msg.ramp(fade).map(|_| ())
            } else {
                msg.to_bytes().map(|_| ())
            };
            if let Err(error) = validation {
                issues.push(CueIssue::error(error.to_string()));
            }
            if msg.port_name.trim().is_empty() {
                issues.push(if prepared { CueIssue::error("MIDI port not configured") } else { CueIssue::warning("MIDI port not configured") });
            } else if !ctx.midi_ports.iter().any(|p| p == &msg.port_name) {
                issues.push(CueIssue::error(format!(
                    "MIDI port not available: \"{}\"",
                    msg.port_name
                )));
            } else if prepared && ctx.midi_ports.iter().filter(|port| *port == &msg.port_name).nth(1).is_some() {
                issues.push(CueIssue::error(format!("More than one MIDI output is named \"{}\"", msg.port_name)));
            }
        }
        issues
    }

    fn serialize(&self) -> Value {
        json!({
            "type": "midi",
            "cue_type": "midi",
            "id": self.id,
            "number": self.number,
            "name": self.name,
            "notes": self.notes,
            "color": self.color,
            "pre_wait_ms": self.pre_wait.as_millis() as u64,
            "post_wait_ms": self.post_wait.as_millis() as u64,
            "continue_mode": self.continue_mode,
            "is_disabled": self.is_disabled,
            "messages": self.messages,
        })
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct MidiCueFactory;

impl CueFactory for MidiCueFactory {
    fn create(&self) -> Box<dyn Cue> {
        Box::new(MidiCue::new())
    }

    fn from_json(&self, value: Value) -> anyhow::Result<Box<dyn Cue>> {
        let mut cue = MidiCue::new();

        if let Some(s) = value.get("id").and_then(|v| v.as_str()) {
            cue.id = s.parse().unwrap_or_else(|_| Uuid::new_v4());
        }
        if let Some(s) = value.get("name").and_then(|v| v.as_str()) {
            cue.name = s.to_string();
        }
        if let Some(s) = value.get("number").and_then(|v| v.as_str()) {
            cue.number = Some(s.to_string());
        }
        if let Some(s) = value.get("notes").and_then(|v| v.as_str()) {
            cue.notes = s.to_string();
        }
        if let Some(ms) = value.get("pre_wait_ms").and_then(|v| v.as_u64()) {
            cue.pre_wait = Duration::from_millis(ms);
        }
        if let Some(ms) = value.get("post_wait_ms").and_then(|v| v.as_u64()) {
            cue.post_wait = Duration::from_millis(ms);
        }
        if let Some(cm) = value.get("continue_mode") {
            if let Ok(mode) = serde_json::from_value(cm.clone()) {
                cue.continue_mode = mode;
            }
        }
        if let Some(col) = value.get("color") {
            if let Ok(color) = serde_json::from_value(col.clone()) {
                cue.color = color;
            }
        }
        if let Some(b) = value.get("is_disabled").and_then(|v| v.as_bool()) {
            cue.is_disabled = b;
        }
        if let Some(Value::Array(arr)) = value.get("messages") {
            let strict = arr.iter().any(|item| {
                item.get("fade").is_some_and(|fade| !fade.is_null())
                    || item["message_type"] == "sysex"
            });
            for item in arr {
                match serde_json::from_value::<MidiMessage>(item.clone()) {
                    Ok(msg) => cue.messages.push(msg),
                    Err(error) if strict => return Err(anyhow!("Invalid MIDI cue with a timed fade or SysEx row: {error}")),
                    Err(e) => log::warn!("MidiCue: skipping invalid message: {e}"),
                }
            }
        }

        Ok(Box::new(cue))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn fade(kind: MidiMessageType, data1: u8, data2: u8, end_value: i16) -> MidiMessage {
        MidiMessage {
            port_name: "Test MIDI receiver".into(),
            message_type: kind,
            channel: 3,
            data1,
            data2,
            msc: None,
            sysex: None,
            fade: Some(MidiFade { duration_ms: 1000, end_value, curve: None }),
        }
    }

    fn recording_sink(bytes: &Arc<Mutex<Vec<Vec<u8>>>>) -> MidiSink {
        let bytes = Arc::clone(bytes);
        Box::new(move |message| {
            bytes.lock().expect("recorded MIDI bytes").push(message.to_vec());
            Ok(())
        })
    }

    #[test]
    fn sysex_preserves_order_and_rejects_malformed_or_oversize_packets() {
        let expected = [0xF0, 0x7D, 0x10, 0x20, 0x30, 0xF7];
        assert_eq!(sysex_bytes("7d10 20\n30").unwrap(), expected);
        assert_eq!(sysex_bytes("f0 7D 10\t20 30f7").unwrap(), expected);
        for invalid in ["", "7D 1", "7D 0X", "7D,10", "F0 7D", "7D F7", "F0 F7",
            "F0 7D F7 F0 7D F7", "F0 7D F8 F7", "7D 80", "00 01"] {
            assert!(sysex_bytes(invalid).is_err(), "accepted {invalid:?}");
        }
        assert_eq!(sysex_bytes("00 20 33").unwrap(), [0xF0, 0x00, 0x20, 0x33, 0xF7]);
        let max_data = "7D".repeat(MAX_SYSEX_MESSAGE_BYTES - 2);
        assert_eq!(sysex_bytes(&max_data).unwrap().len(), MAX_SYSEX_MESSAGE_BYTES);
        assert!(sysex_bytes(&(max_data + "01")).is_err());
        assert!(sysex_bytes(&" ".repeat(MAX_SYSEX_TEXT_BYTES + 1)).is_err());
    }

    #[test]
    fn sysex_runs_once_after_pre_wait_and_stays_in_message_order_after_reload() {
        let now = Instant::now();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let authored = json!({"pre_wait_ms": 300, "messages": [
            {"port_name":"Recording sink", "message_type":"sysex", "channel":1, "data1":0, "data2":0, "sysex":"7D 10 20"},
            {"port_name":"Recording sink", "message_type":"note_on", "channel":1, "data1":60, "data2":100},
        ]});
        let saved = MidiCueFactory.from_json(authored).unwrap().serialize();
        let restored = MidiCueFactory.from_json(saved.clone()).unwrap().serialize();
        let mut cue = MidiCue::new();
        cue.pre_wait = Duration::from_millis(restored["pre_wait_ms"].as_u64().unwrap());
        cue.messages = serde_json::from_value(restored["messages"].clone()).unwrap();
        let mut events = Vec::new();
        cue.go_with(|| now, |name| {
            assert_eq!(name, "Recording sink");
            Ok(recording_sink(&recorded))
        }, |event| events.push(event)).unwrap();
        assert!(!cue.is_time_complete());
        cue.tick_at(now + Duration::from_millis(299), |event| events.push(event));
        assert!(recorded.lock().unwrap().is_empty());
        cue.tick_at(now + Duration::from_millis(300), |event| events.push(event));
        cue.tick_at(now + Duration::from_millis(330), |event| events.push(event));
        assert_eq!(*recorded.lock().unwrap(), [vec![0xF0, 0x7D, 0x10, 0x20, 0xF7], vec![0x90, 60, 100]]);
        assert!(matches!(events.as_slice(), [CueEvent::ActionStarted { .. }, CueEvent::ActionCompleted { .. }]));
        assert!(cue.is_time_complete());

        cue.reset().unwrap();
        cue.pre_wait = Duration::ZERO;
        cue.go_with(|| now, |_| Ok(recording_sink(&recorded)), |_| {}).unwrap();
        assert_eq!(recorded.lock().unwrap().len(), 4);
    }

    #[test]
    fn invalid_or_unroutable_sysex_keeps_mixed_midi_cue_in_standby() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut cue = MidiCue::new();
        cue.messages = serde_json::from_value(json!([
            {"port_name":"Recording sink", "message_type":"note_on", "channel":1, "data1":60, "data2":100},
            {"port_name":"Other recording sink", "message_type":"sysex", "channel":1, "data1":0, "data2":0, "sysex":"F0 7D"},
        ])).unwrap();
        let no_event = |_| panic!("invalid MIDI cue started");
        assert!(cue.go_with(Instant::now, |_| panic!("invalid bytes opened a port"), no_event).is_err());
        cue.messages[1].sysex = Some("7D".into());
        assert!(cue.go_with(Instant::now, |name| {
            if name == "Recording sink" { Ok(recording_sink(&recorded)) }
            else { Err(anyhow!("No unique SysEx output found")) }
        }, no_event).is_err());
        assert_eq!(cue.state(), CueState::Standby);
        assert!(recorded.lock().unwrap().is_empty());
        crate::health::clear("midi:Other recording sink");
    }

    #[test]
    fn all_four_fades_encode_their_wire_fields_and_end_once_at_the_deadline() {
        use MidiMessageType::*;
        let cases: [(MidiMessage, &[u8], &[u8]); 4] = [
            (fade(ControlChange, 7, 119, 4), &[0xB2, 7, 119], &[0xB2, 7, 4]),
            (fade(KeyPressure, 74, 0, 127), &[0xA2, 74, 0], &[0xA2, 74, 127]),
            (fade(ChannelPressure, 10, 0, 3), &[0xD2, 10], &[0xD2, 3]),
            (fade(PitchBend, 0, 0, 8191), &[0xE2, 0, 0], &[0xE2, 127, 127]),
        ];
        for (message, start, end) in cases {
            let mut ramp = message.ramp(message.fade.as_ref().expect("test fade")).expect("valid ramp");
            let (bytes, len) = ramp.sample(Duration::ZERO).expect("start");
            assert_eq!(&bytes[..len], start);
            let (bytes, len) = ramp.sample(Duration::from_millis(999)).expect("last intermediate");
            assert_ne!(&bytes[..len], end);
            let (bytes, len) = ramp.sample(Duration::from_secs(1)).expect("end");
            assert_eq!(&bytes[..len], end);
            assert!(ramp.sample(Duration::from_secs(2)).is_none());
        }
    }

    #[test]
    fn midi_curves_follow_the_authored_turns_without_sending_the_target_early() {
        use crate::cue::curve::{CurveKind, CurvePoint};

        let now = Instant::now();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut cue = MidiCue::new();
        let mut cc = fade(MidiMessageType::ControlChange, 7, 20, 100);
        let mut custom = CurveShape::of_kind(CurveKind::Linear);
        custom.points = vec![CurvePoint::new(0.37, 1.0), CurvePoint::new(0.65, 0.45)];
        cc.fade.as_mut().unwrap().curve = Some(custom);
        let mut pressure = fade(MidiMessageType::ChannelPressure, 100, 0, 20);
        pressure.fade.as_mut().unwrap().curve = Some(CurveShape::of_kind(CurveKind::Exponential));
        cue.messages = vec![cc, pressure];

        cue.go_with(|| now, |_| Ok(recording_sink(&recorded)), |_| {}).unwrap();
        for ms in [370, 500, 650, 1000, 1200] {
            cue.tick_at(now + Duration::from_millis(ms), |_| {});
        }
        let bytes = recorded.lock().unwrap();
        let cc_values: Vec<_> = bytes.iter().filter(|packet| packet[0] == 0xB2).map(|packet| packet[2]).collect();
        let pressure_values: Vec<_> = bytes.iter().filter(|packet| packet[0] == 0xD2).map(|packet| packet[1]).collect();
        assert_eq!(cc_values, [20, 99, 80, 56, 100]);
        assert_eq!(pressure_values, [100, 97, 94, 87, 20]);
    }

    #[test]
    fn pause_preserves_pre_wait_and_action_time_without_resending_one_shots() {
        let now = Instant::now();
        let at = |ms| now + Duration::from_millis(ms);
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut cue = MidiCue::new();
        cue.pre_wait = Duration::from_millis(300);
        let mut immediate = fade(MidiMessageType::ProgramChange, 19, 0, 0);
        immediate.fade = None;
        cue.messages = vec![immediate, fade(MidiMessageType::ControlChange, 7, 0, 100)];
        let mut events = Vec::new();
        cue.go_with(|| now, |_| Ok(recording_sink(&recorded)), |event| events.push(event)).expect("GO");
        cue.pause_at(at(200));
        cue.tick_at(at(700), |event| events.push(event));
        assert!(!cue.is_action_started());
        assert_eq!(cue.elapsed(), Duration::from_millis(200));
        assert!(recorded.lock().expect("recording").is_empty());
        cue.resume_at(at(700));
        cue.tick_at(at(799), |event| events.push(event));
        assert!(!cue.is_action_started());
        cue.tick_at(at(800), |event| events.push(event));
        cue.tick_at(at(1300), |event| events.push(event));
        cue.pause_at(at(1300));
        cue.tick_at(at(4000), |event| events.push(event));
        assert_eq!(cue.action_elapsed(), Duration::from_millis(500));
        cue.resume_at(at(4000));
        cue.tick_at(at(4500), |event| events.push(event));
        cue.tick_at(at(4501), |event| events.push(event));

        assert_eq!(*recorded.lock().expect("recording"), [
            vec![0xC2, 19], vec![0xB2, 7, 0], vec![0xB2, 7, 50], vec![0xB2, 7, 100],
        ]);
        assert!(matches!(events.as_slice(), [CueEvent::ActionStarted { .. }, CueEvent::ActionCompleted { .. }]));
    }

    #[test]
    fn late_tick_skips_overdue_samples_and_flat_fade_sends_one_value() {
        let now = Instant::now();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut cue = MidiCue::new();
        cue.messages = vec![
            fade(MidiMessageType::ControlChange, 1, 0, 127),
            fade(MidiMessageType::ControlChange, 2, 25, 25),
        ];
        cue.go_with(|| now, |_| Ok(recording_sink(&recorded)), |_| {}).expect("GO");
        cue.tick_at(now + Duration::from_secs(30), |_| {});
        assert_eq!(*recorded.lock().expect("recording"), [
            vec![0xB2, 1, 0], vec![0xB2, 2, 25], vec![0xB2, 1, 127],
        ]);
        assert!(cue.run.as_ref().expect("armed run").finished);
    }

    #[test]
    fn reset_drops_future_sends_and_edit_keeps_the_original_running_program() {
        let now = Instant::now();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut cue = MidiCue::new();
        cue.messages.push(fade(MidiMessageType::PitchBend, 0, 64, -8192));
        cue.go_with(|| now, |_| Ok(recording_sink(&recorded)), |_| {}).expect("GO");
        cue.mark_auto_continue_fired();

        let mut edited = MidiCue::new();
        edited.messages.push(fade(MidiMessageType::PitchBend, 0, 64, 8191));
        edited.inherit_playback(&mut cue);
        assert!(edited.is_auto_continue_fired());
        edited.tick_at(now + Duration::from_millis(500), |_| {});
        edited.reset().expect("cancel remaining output");
        edited.tick_at(now + Duration::from_millis(1500), |_| panic!("canceled cue ended"));
        assert_eq!(*recorded.lock().expect("recording"), [
            vec![0xE2, 0, 64], vec![0xE2, 0, 32],
        ]);
    }

    #[test]
    fn invalid_fades_or_missing_ports_do_not_dispatch_other_rows() {
        use crate::cue::curve::{CurveKind, CurvePoint};

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut cue = MidiCue::new();
        cue.messages = vec![
            fade(MidiMessageType::ControlChange, 1, 0, 127),
            fade(MidiMessageType::ControlChange, 123, 0, 127),
        ];
        let no_event = |_| panic!("a rejected MIDI GO emitted a cue event");
        let error = cue.go_with(Instant::now, |_| panic!("invalid rows must not open MIDI"), no_event);
        assert!(error.is_err());
        cue.messages[1].data1 = 2;
        let mut custom = CurveShape::of_kind(CurveKind::Linear);
        custom.points = vec![CurvePoint { t: 0.5, v: 2.0 }];
        cue.messages[1].fade.as_mut().unwrap().curve = Some(custom);
        let error = cue.go_with(Instant::now, |_| panic!("invalid curves must not open MIDI"), no_event);
        assert!(error.is_err());
        cue.messages[1].fade.as_mut().unwrap().curve = None;
        cue.messages[1].port_name = "Missing recorded MIDI receiver".into();
        let error = cue.go_with(Instant::now, |port| {
            if port == "Test MIDI receiver" { Ok(recording_sink(&recorded)) }
            else { Err(anyhow!("missing port")) }
        }, no_event);
        assert!(error.is_err());
        assert_eq!(cue.state(), CueState::Standby);
        assert!(recorded.lock().expect("recording").is_empty());
        crate::health::clear("midi:Missing recorded MIDI receiver");
    }

    #[test]
    fn disconnected_port_is_never_retried_during_the_same_fade() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let now = Instant::now();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut cue = MidiCue::new();
        let mut message = fade(MidiMessageType::ChannelPressure, 10, 0, 120);
        message.port_name = "Failed recorded MIDI receiver".into();
        cue.messages.push(message);
        cue.go_with(|| now, |_| {
            let calls = Arc::clone(&calls);
            Ok(Box::new(move |_| {
                if calls.fetch_add(1, Ordering::Relaxed) == 0 { Ok(()) }
                else { Err(anyhow!("disconnected")) }
            }))
        }, |_| {}).expect("GO");
        for ms in [330, 660, 990, 1320, 1650] {
            cue.tick_at(now + Duration::from_millis(ms), |_| {});
        }
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert!(cue.run.as_ref().expect("armed run").finished);
        crate::health::clear("midi:Failed recorded MIDI receiver");
    }

    #[test]
    fn fade_serializes_and_a_malformed_timed_row_is_not_dropped() {
        use crate::cue::curve::{CurveKind, CurvePoint};

        let mut cue = MidiCue::new();
        cue.messages.push(fade(MidiMessageType::PitchBend, 0, 64, -128));
        let legacy = cue.serialize();
        assert_eq!(legacy["messages"][0]["fade"], json!({"duration_ms": 1000, "end_value": -128}));
        let reloaded = MidiCueFactory.from_json(legacy.clone()).expect("reopen linear fade");
        assert_eq!(reloaded.serialize(), legacy);

        let mut shape = CurveShape::of_kind(CurveKind::Linear);
        shape.points = vec![CurvePoint::new(0.4, 0.7)];
        shape.bends = vec![0.2, -0.3];
        cue.messages[0].fade.as_mut().unwrap().curve = Some(shape);
        let mut json = cue.serialize();
        let reloaded = MidiCueFactory.from_json(json.clone()).expect("reopen custom fade");
        assert_eq!(reloaded.duration(), Some(Duration::from_secs(1)));
        assert_eq!(reloaded.serialize(), json);
        json["messages"][0]["fade"]["duration_ms"] = json!("1.0");
        assert!(MidiCueFactory.from_json(json).is_err());
    }

    #[test]
    fn note_on_bytes() {
        let msg = MidiMessage {
            msc: None,
            sysex: None,
            fade: None,
            port_name: String::new(),
            message_type: MidiMessageType::NoteOn,
            channel: 1,
            data1: 60,
            data2: 127,
        };
        assert_eq!(msg.to_bytes().unwrap(), vec![0x90, 60, 127]);
    }

    #[test]
    fn note_on_channel_16() {
        let msg = MidiMessage {
            msc: None,
            sysex: None,
            fade: None,
            port_name: String::new(),
            message_type: MidiMessageType::NoteOn,
            channel: 16,
            data1: 60,
            data2: 64,
        };
        assert_eq!(msg.to_bytes().unwrap(), vec![0x9F, 60, 64]);
    }

    #[test]
    fn program_change_two_bytes() {
        let msg = MidiMessage {
            msc: None,
            sysex: None,
            fade: None,
            port_name: String::new(),
            message_type: MidiMessageType::ProgramChange,
            channel: 1,
            data1: 5,
            data2: 0,
        };
        assert_eq!(msg.to_bytes().unwrap(), vec![0xC0, 5]);
    }

    #[test]
    fn control_change_bytes() {
        let msg = MidiMessage {
            msc: None,
            sysex: None,
            fade: None,
            port_name: String::new(),
            message_type: MidiMessageType::ControlChange,
            channel: 3,
            data1: 7,
            data2: 100,
        };
        assert_eq!(msg.to_bytes().unwrap(), vec![0xB2, 7, 100]);
    }

    #[test]
    fn validate_flags_absent_and_unconfigured_ports() {
        use crate::cue::validation::{Severity, ValidationContext};
        use std::collections::HashSet;
        let ctx = ValidationContext {
            all_cue_ids: HashSet::new(),
            fixture_ids: HashSet::new(),
            fixture_group_ids: HashSet::new(),
            osc_patch_ids: HashSet::new(),
            output_patch_ids: HashSet::new(),
            midi_ports: vec!["Real Port".to_string()],
            video_output_ids: Default::default(),
            visual_cue_ids: Default::default(),
            rotation_cue_ids: Default::default(),
        };

        let mut cue = MidiCue::new();
        // A port that is not present on this machine → an error.
        cue.messages.push(MidiMessage {
            msc: None,
            sysex: None,
            fade: None,
            port_name: "Ghost Port".to_string(),
            message_type: MidiMessageType::NoteOn,
            channel: 1, data1: 60, data2: 100,
        });
        let issues = cue.validate(&ctx);
        assert!(issues.iter().any(|i| i.severity == Severity::Error));

        // A present port → no issue.
        cue.messages = vec![MidiMessage {
            msc: None,
            sysex: None,
            fade: None,
            port_name: "Real Port".to_string(),
            message_type: MidiMessageType::NoteOn,
            channel: 1, data1: 60, data2: 100,
        }];
        assert!(cue.validate(&ctx).is_empty());
    }

    #[test]
    fn serialize_roundtrip() {
        let factory = MidiCueFactory;
        let mut cue = MidiCue::new();
        cue.set_name("My MIDI".to_string());
        cue.messages.push(MidiMessage {
            msc: None,
            sysex: None,
            fade: None,
            port_name: "Test Port".to_string(),
            message_type: MidiMessageType::NoteOn,
            channel: 1,
            data1: 60,
            data2: 100,
        });

        let json = cue.serialize();
        assert_eq!(json["name"], "My MIDI");
        assert_eq!(json["messages"].as_array().unwrap().len(), 1);

        let rebuilt = factory.from_json(json).unwrap();
        assert_eq!(rebuilt.name(), "My MIDI");
        assert_eq!(rebuilt.cue_type(), CueType::Midi);
    }

    #[test]
    fn msc_survives_cue_save_and_load_with_unchanged_wire_bytes() {
        let cue = MidiCueFactory.from_json(json!({"messages":[{"port_name":"Monitor",
            "message_type":"msc","channel":1,"data1":0,"data2":0,
            "msc":{"device_id":3,"command_format":48,"command":"resume","q_number":"1.2"}}]})).unwrap();
        let reloaded = MidiCueFactory.from_json(cue.serialize()).unwrap().serialize();
        let msg: MidiMessage = serde_json::from_value(reloaded["messages"][0].clone()).unwrap();
        assert_eq!(msg.to_bytes().unwrap(), [0xf0,0x7f,3,2,48,3,49,46,50,0xf7]);
    }

    #[test]
    fn cue_type_is_midi() {
        assert_eq!(MidiCue::new().cue_type(), CueType::Midi);
    }
}
