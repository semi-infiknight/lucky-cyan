//! Timecode receiver — decodes incoming MTC (MIDI Timecode) and LTC
//! (Linear/Longitudinal Timecode from an audio input) into [`TcPosition`]
//! events and distributes them to the dispatcher.
//!
//! **MTC** — spawns a `midir::MidiInput` connection on a background thread.
//! Handles both quarter-frame messages (8 messages → advance 2 frames) and
//! full-frame SysEx.  A **flywheel** (software PLL) interpolates the position
//! between quarter-frame pairs so the dispatcher always has a current value.
//!
//! **LTC** — reads f32 samples from an audio [`InputFeed`] ring and passes
//! them through the biphase-mark decoder in [`crate::engine::ltc`].
//!
//! Both paths emit [`TcEvent`]s for the show event loop. Each receiver owns
//! its connection and channel so a source change isolates pending events.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use ringbuf::traits::{Consumer, Observer};

use super::audio_input::open_input;
use super::ltc::LtcDecoder;
use super::timecode_types::{TcEvent, TcPosition, TcRate};

#[derive(Clone, Copy)]
pub(crate) struct TcObservation {
    pub(crate) event: TcEvent,
    pub(crate) at: Instant,
}

// ---------------------------------------------------------------------------
// Public configuration
// ---------------------------------------------------------------------------

/// Which physical source feeds this receiver.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TcSource {
    /// MTC via a MIDI input port.
    #[default]
    Mtc,
    /// LTC decoded from an audio input (requires an Input Patch).
    Ltc,
}

/// Machine-level timecode receiver configuration (stored in machine-config,
/// like the audio device — source/port are hardware-specific).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TcReceiverConfig {
    /// Which physical source to listen to.
    #[serde(default)]
    pub source: TcSource,
    /// MIDI input port name (for MTC).  `None` = first available port.
    #[serde(default)]
    pub midi_port: Option<String>,
    /// Input patch device id (for LTC).  `None` = default input.
    #[serde(default)]
    pub ltc_device_id: Option<String>,
}

impl Default for TcReceiverConfig {
    fn default() -> Self {
        Self { source: TcSource::Mtc, midi_port: None, ltc_device_id: None }
    }
}

// ---------------------------------------------------------------------------
// MTC quarter-frame decoder
// ---------------------------------------------------------------------------

/// Assembles 8 MTC quarter-frame messages into a complete [`TcPosition`].
///
/// MTC sends one nibble per quarter-frame (2 frames = 8 messages).
/// The full frame becomes valid after message 7 arrives.
#[derive(Default)]
pub(crate) struct MtcAssembler {
    nibbles: [u8; 8],
    count:   u8,
}

impl MtcAssembler {
    /// Feed one quarter-frame byte (the data byte of a 0xF1 message).
    /// Returns `Some(TcPosition)` when a complete frame has been assembled
    /// (eight consecutive pieces in forward order).
    pub fn push_quarter_frame(&mut self, data: u8) -> Option<TcPosition> {
        let piece = (data >> 4) & 0x07;
        if data & 0x80 != 0 || (piece != 0 && piece != self.count) {
            self.count = 0;
            return None;
        }
        self.nibbles[piece as usize] = data & 0x0F;
        self.count = (piece + 1) % 8;
        (piece == 7).then(|| self.assemble()).filter(|pos| pos.is_valid())
    }

    /// Feed a full-frame SysEx payload (F0 7F 7F 01 01 hh mm ss ff F7).
    /// The four data bytes are the last four bytes before F7.
    pub fn push_full_frame(&mut self, hh: u8, mm: u8, ss: u8, ff: u8) -> Option<TcPosition> {
        let rate = match (hh >> 5) & 0x03 {
            0 => TcRate::Fps24,
            1 => TcRate::Fps25,
            2 => TcRate::Fps2997Df,
            _ => TcRate::Fps30,
        };
        self.count = 0;
        let position = TcPosition::new(hh & 0x1F, mm, ss, ff, rate);
        (hh < 128 && position.is_valid()).then_some(position)
    }

    fn assemble(&self) -> TcPosition {
        let n = &self.nibbles;
        let frames  =  n[0] | ((n[1] & 0x01) << 4);
        let seconds =  n[2] | ((n[3] & 0x03) << 4);
        let minutes =  n[4] | ((n[5] & 0x03) << 4);
        let hours   =  n[6] | ((n[7] & 0x01) << 4);
        let rate = match (n[7] >> 1) & 0x03 {
            0 => TcRate::Fps24,
            1 => TcRate::Fps25,
            2 => TcRate::Fps2997Df,
            _ => TcRate::Fps30,
        };
        TcPosition::new(hours, minutes, seconds, frames, rate)
    }
}

// ---------------------------------------------------------------------------
// Flywheel / interpolator
// ---------------------------------------------------------------------------

/// Tracks the last received `TcPosition` and interpolates the current
/// position from the wall-clock elapsed time since it arrived.
pub(crate) struct TcFlywheel {
    last_pos:  Option<TcPosition>,
    last_wall: Option<Instant>,
    /// How long we coast before declaring TC stopped.
    freewheel: Duration,
}

impl TcFlywheel {
    pub fn new(freewheel_ms: u32) -> Self {
        Self {
            last_pos:  None,
            last_wall: None,
            freewheel: Duration::from_millis(freewheel_ms as u64),
        }
    }

    fn update(&mut self, pos: TcPosition, now: Instant) {
        self.last_pos = Some(pos);
        self.last_wall = Some(now);
    }

    /// Current interpolated position, or `None` if the freewheel has expired.
    pub fn current(&self, now: Instant) -> Option<TcPosition> {
        let (pos, wall) = self.last_pos.zip(self.last_wall)?;
        let elapsed = now.saturating_duration_since(wall);
        if elapsed > self.freewheel {
            return None;
        }
        // Interpolate: advance by elapsed time.
        let extra_frames = (elapsed.as_micros() as u64) / pos.rate.frame_us();
        let advanced = TcPosition::from_frame_number(
            pos.to_frame_number().saturating_add(extra_frames),
            pos.rate,
        );
        Some(advanced)
    }

    fn publish(&mut self, tx: &Sender<TcObservation>, event: Option<TcEvent>, now: Instant) {
        let expired = self.last_wall
            .is_some_and(|wall| now.saturating_duration_since(wall) > self.freewheel);
        if expired {
            self.last_wall = None;
            let _ = tx.send(TcObservation { event: TcEvent::Stopped, at: now });
        }
        if let Some(event) = event {
            match event {
                TcEvent::Position(pos, _) => self.update(pos, now),
                TcEvent::Located(_) | TcEvent::Stopped => self.last_wall = None,
            }
            let _ = tx.send(TcObservation { event, at: now });
        }
    }
}

// ---------------------------------------------------------------------------
// Receiver handle
// ---------------------------------------------------------------------------

/// Enumerate available MIDI input port names (for UI dropdowns).
pub fn list_midi_input_ports() -> Vec<String> {
    match midir::MidiInput::new("Inkue-tc-list") {
        Ok(inp) => inp
            .ports()
            .iter()
            .filter_map(|p| inp.port_name(p).ok())
            .collect(),
        Err(e) => {
            log::warn!("TC: failed to enumerate MIDI input ports: {e}");
            Vec::new()
        }
    }
}

/// Handle to the timecode receiver subsystem.
///
/// Internally spawns a background thread that owns the `midir` connection
/// (or the LTC audio decode loop) and publishes [`TcEvent`]s to the channel
/// returned by [`TimecodeReceiver::subscribe`].
pub struct TimecodeReceiver {
    event_rx:   Receiver<TcObservation>,
    config:     TcReceiverConfig,
    /// Set to `true` to ask the receiver thread to shut down.
    shutdown:   Arc<AtomicBool>,
    flywheel:   Arc<Mutex<TcFlywheel>>,
}

impl TimecodeReceiver {
    #[cfg(test)]
    pub(crate) fn test_channel() -> (Arc<Self>, Sender<TcObservation>) {
        let (tx, event_rx) = crossbeam_channel::unbounded();
        let receiver = Arc::new(Self {
            event_rx,
            config: TcReceiverConfig::default(),
            shutdown: Arc::new(AtomicBool::new(false)),
            flywheel: Arc::new(Mutex::new(TcFlywheel::new(500))),
        });
        (receiver, tx)
    }

    /// Create a receiver from `config` and start the background thread.
    pub fn new(config: TcReceiverConfig) -> Arc<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<TcObservation>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let flywheel = Arc::new(Mutex::new(TcFlywheel::new(500)));

        let receiver = Arc::new(Self {
            event_rx: rx,
            config: config.clone(),
            shutdown: Arc::clone(&shutdown),
            flywheel: Arc::clone(&flywheel),
        });

        Self::start_thread(config, tx, Arc::clone(&shutdown), Arc::clone(&flywheel));
        receiver
    }

    /// Drain the events queued by this connection in their arrival order.
    pub(crate) fn events(&self) -> impl Iterator<Item = TcObservation> + '_ {
        self.event_rx.try_iter()
    }

    /// Hardware route for this connection.
    pub(crate) fn config(&self) -> &TcReceiverConfig {
        &self.config
    }

    /// Current interpolated timecode position (for the UI status indicator).
    /// Returns `None` when TC is stationary or freewheel has expired.
    pub fn current_position(&self) -> Option<TcPosition> {
        self.flywheel.lock().ok()?.current(Instant::now())
    }

    // ── Internal ─────────────────────────────────────────────────────────

    fn start_thread(
        config:   TcReceiverConfig,
        tx:       Sender<TcObservation>,
        shutdown: Arc<AtomicBool>,
        flywheel: Arc<Mutex<TcFlywheel>>,
    ) {
        match config.source {
            TcSource::Mtc => {
                std::thread::Builder::new()
                    .name("inkue-tc-mtc".into())
                    .spawn(move || mtc_thread(config.midi_port, tx, shutdown, flywheel))
                    .expect("failed to spawn MTC receiver thread");
            }
            TcSource::Ltc => {
                std::thread::Builder::new()
                    .name("inkue-tc-ltc".into())
                    .spawn(move || ltc_thread(config.ltc_device_id, tx, shutdown, flywheel))
                    .expect("failed to spawn LTC receiver thread");
            }
        }
    }
}

impl Drop for TimecodeReceiver {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// MTC receive thread
// ---------------------------------------------------------------------------

fn mtc_thread(
    port_name: Option<String>,
    tx:        Sender<TcObservation>,
    shutdown:  Arc<AtomicBool>,
    flywheel:  Arc<Mutex<TcFlywheel>>,
) {
    let Ok(midi_in) = midir::MidiInput::new("Inkue-tc") else {
        log::error!("TC: failed to create MIDI input");
        return;
    };

    let ports = midi_in.ports();
    if ports.is_empty() {
        log::warn!("TC: no MIDI input ports found");
        return;
    }

    let port = if let Some(name) = &port_name {
        ports.iter().find(|p| midi_in.port_name(p).ok().as_deref() == Some(name))
    } else {
        ports.first()
    };

    let Some(port) = port else {
        log::warn!("TC: MTC port '{:?}' not found", port_name);
        return;
    };

    let port_display = midi_in.port_name(port).unwrap_or_default();
    log::info!("TC: MTC listening on '{port_display}'");

    let tx2       = tx.clone();
    let flywheel2 = Arc::clone(&flywheel);
    let shutdown2 = Arc::clone(&shutdown);

    let connection = midi_in.connect(
        port,
        "inkue-tc",
        move |_stamp, message, assembler| {
            if shutdown2.load(Ordering::Relaxed) { return; }
            if !crate::engine::overrides::OVERRIDES.timecode_input() { return; }
            let Some(event) = decode_mtc_message(message, assembler) else { return };
            if let Ok(mut clock) = flywheel2.lock() {
                clock.publish(&tx2, Some(event), Instant::now());
            }
        },
        MtcAssembler::default(),
    );
    let _connection = match connection {
        Ok(connection) => connection,
        Err(error) => {
            log::error!("TC: could not open MIDI input '{port_display}': {}", error.kind());
            return;
        }
    };

    // Keep the connection alive until shutdown is requested, polling every 50ms.
    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(mut clock) = flywheel.lock() {
            clock.publish(&tx, None, Instant::now());
        }
    }
    log::info!("TC: MTC thread stopped");
}

fn decode_mtc_message(msg: &[u8], asm: &mut MtcAssembler) -> Option<TcEvent> {
    match msg {
        // Quarter-frame: F1 dd
        [0xF1, data] => asm.push_quarter_frame(*data).map(|pos| TcEvent::Position(pos, 2)),
        // Full Frame applies to any incoming device ID on the selected input.
        [0xF0, 0x7F, device, 0x01, 0x01, hh, mm, ss, ff, 0xF7] if *device < 128 => {
            asm.push_full_frame(*hh, *mm, *ss, *ff).map(TcEvent::Located)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// LTC receive thread
// ---------------------------------------------------------------------------

fn ltc_thread(
    device_id: Option<String>,
    tx:        Sender<TcObservation>,
    shutdown:  Arc<AtomicBool>,
    flywheel:  Arc<Mutex<TcFlywheel>>,
) {
    let (capture, mut cons) = match open_input(device_id.as_deref(), 0) {
        Ok(c) => c,
        Err(e) => {
            log::error!("TC: LTC failed to open input device {device_id:?}: {e}");
            return;
        }
    };
    let sample_rate = capture.sample_rate;
    let channels = capture.channels.max(1) as usize;
    // 30 fps seeds the decoder's bit-clock estimate; it adapts to the real rate.
    let mut decoder = LtcDecoder::new(sample_rate, 30);
    let mut mono: Vec<f32> = Vec::with_capacity(4096);

    log::info!(
        "TC: LTC listening on '{}' ({sample_rate} Hz, {channels} ch)",
        capture.device_id
    );

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Drain whole interleaved frames, keeping channel 0 for decoding.
        mono.clear();
        while cons.occupied_len() >= channels {
            let mut frame0 = 0.0_f32;
            for c in 0..channels {
                if let Some(s) = cons.try_pop() {
                    if c == 0 {
                        frame0 = s;
                    }
                }
            }
            mono.push(frame0);
        }

        let positions = if crate::engine::overrides::OVERRIDES.timecode_input() {
            decoder.push(&mono)
        } else {
            Vec::new()
        };
        if let Ok(mut clock) = flywheel.lock() {
            let now = Instant::now();
            clock.publish(&tx, None, now);
            for pos in positions.into_iter().filter(|pos| pos.is_valid()) {
                clock.publish(&tx, Some(TcEvent::Position(pos, 1)), now);
            }
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    drop(capture);
    log::info!("TC: LTC thread stopped");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::timecode_types::TcRate;

    #[test]
    fn quarter_frame_assembles_known_position() {
        let expected = TcPosition::new(1, 2, 3, 4, TcRate::Fps30);
        let mut asm = MtcAssembler::default();
        let f = expected.frames;
        let s = expected.seconds;
        let m = expected.minutes;
        let h = expected.hours;
        let rate_bits: u8 = 3; // Fps30
        let qf: [u8; 8] = [
            f & 0x0F,
            (1 << 4) | ((f >> 4) & 0x01),
            (2 << 4) | (s & 0x0F),
            (3 << 4) | ((s >> 4) & 0x03),
            (4 << 4) | (m & 0x0F),
            (5 << 4) | ((m >> 4) & 0x03),
            (6 << 4) | (h & 0x0F),
            (7 << 4) | (((h >> 4) & 0x01) | (rate_bits << 1)),
        ];
        let mut result = None;
        for byte in qf {
            result = asm.push_quarter_frame(byte);
        }
        let got = result.expect("should assemble on 8th QF");
        assert_eq!(got.hours,   expected.hours);
        assert_eq!(got.minutes, expected.minutes);
        assert_eq!(got.seconds, expected.seconds);
        assert_eq!(got.frames,  expected.frames);
        assert_eq!(got.rate,    expected.rate);
    }

    #[test]
    fn full_frame_sysex_parses_correctly() {
        let mut asm = MtcAssembler::default();
        // 01:02:03:04 @ 29.97df (rate_bits=2 → hh = 2<<5 | 1 = 0x41)
        let event = decode_mtc_message(&[0xF0, 0x7F, 0x04, 1, 1, 0x41, 2, 3, 4, 0xF7], &mut asm);
        let pos = TcPosition::new(1, 2, 3, 4, TcRate::Fps2997Df);
        assert_eq!(event, Some(TcEvent::Located(pos)));
    }

    #[test]
    fn locate_discards_partial_and_reverse_quarter_frames() {
        use crate::engine::timecode_generator::mtc_quarter_frames;

        let mut asm = MtcAssembler::default();
        let pos = TcPosition::new(1, 2, 3, 4, TcRate::Fps25);
        let pieces = mtc_quarter_frames(pos);
        for &piece in &pieces[..4] {
            assert_eq!(decode_mtc_message(&[0xF1, piece], &mut asm), None);
        }
        assert_eq!(
            decode_mtc_message(&[0xF0, 0x7F, 0x7F, 1, 1, 0x21, 2, 3, 4, 0xF7], &mut asm),
            Some(TcEvent::Located(pos)),
        );
        for &piece in pieces[4..].iter().chain(pieces.iter().rev()) {
            assert_eq!(decode_mtc_message(&[0xF1, piece], &mut asm), None);
        }
        for &piece in &pieces[..7] {
            assert_eq!(decode_mtc_message(&[0xF1, piece], &mut asm), None);
        }
        assert_eq!(decode_mtc_message(&[0xF1, pieces[7]], &mut asm), Some(TcEvent::Position(pos, 2)));
    }

    #[test]
    fn flywheel_locate_is_stationary_and_expiry_precedes_recovered_frame() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut fw = TcFlywheel::new(500);
        let now = Instant::now();
        let pos = TcPosition::new(0, 0, 0, 0, TcRate::Fps25);
        let next = TcPosition::new(0, 0, 0, 2, TcRate::Fps25);
        fw.publish(&tx, Some(TcEvent::Located(pos)), now);
        assert_eq!(fw.current(now + Duration::from_millis(100)), None);
        fw.publish(&tx, Some(TcEvent::Position(pos, 2)), now);
        assert_eq!(fw.current(now + Duration::from_millis(80)), Some(next));
        let later = now + Duration::from_millis(501);
        assert_eq!(fw.current(later), None);
        // Querying the widget before the receiver wakes must preserve Stopped.
        fw.publish(&tx, Some(TcEvent::Position(next, 2)), later);
        fw.publish(&tx, None, later);
        assert_eq!(rx.try_iter().map(|observation| observation.event).collect::<Vec<_>>(), [
            TcEvent::Located(pos),
            TcEvent::Position(pos, 2),
            TcEvent::Stopped,
            TcEvent::Position(next, 2),
        ]);
    }
}
