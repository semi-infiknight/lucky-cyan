//! Lock-loss decisions for a list's received clock, separate from frame triggers.

use std::time::{Duration, Instant};

use anyhow::Result;
use uuid::Uuid;

use crate::{
    cue::{
        audio_cue::AudioCue, context::CueContext, traits::Cue, types::{CueState, CueType},
        video_cue::VideoCue, wait_cue::WaitCue,
    },
    engine::timecode_types::{TcOnStop, TcRate},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClockEdge {
    Lost(Instant),
    Returned(Instant),
}

#[derive(Default)]
pub(crate) struct ClockLock {
    received: Option<(Instant, Duration)>,
    lost: bool,
}

impl ClockLock {
    pub(crate) fn receive(&mut self, at: Instant, rate: TcRate, step: u8) -> Option<ClockEdge> {
        // Ordinary MTC observations represent two frames and LTC represents one.
        // Allow a missed observation before declaring loss even at zero freewheel.
        let cadence = Duration::from_micros(2 * rate.frame_us() * u64::from(step.max(1)));
        self.received = Some((at, cadence));
        std::mem::take(&mut self.lost).then_some(ClockEdge::Returned(at))
    }

    pub(crate) fn expire(&mut self, now: Instant, freewheel_ms: u32) -> Option<ClockEdge> {
        let (at, cadence) = self.received?;
        let delay = cadence.max(Duration::from_millis(u64::from(freewheel_ms.min(2000))));
        if now.saturating_duration_since(at) < delay {
            return None;
        }
        self.received = None;
        self.lost = true;
        Some(ClockEdge::Lost(at + delay))
    }

    /// A locate suspends gap inference until the source supplies a moving frame.
    /// A prior real loss can still be recovered by the first received frame.
    pub(crate) fn locate(&mut self) {
        self.received = None;
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ClockRun {
    Controlled,
    PausedByClock { at: Instant },
    /// The desk now owns a hold that had been controlled by the clock.
    PausedByOperator,
}

impl ClockRun {
    pub(crate) fn leave_paused(run: &mut Option<(Uuid, Self)>) {
        if let Some((_, run)) = run {
            *run = Self::PausedByOperator;
        }
    }
}

pub(crate) trait ClockMediaCue: Cue {
    fn clock_run(&mut self) -> &mut Option<(Uuid, ClockRun)>;
}

fn media_cue(cue: &mut dyn Cue) -> Option<&mut dyn ClockMediaCue> {
    let kind = cue.cue_type();
    let cue = cue as &mut dyn std::any::Any;
    match kind {
        CueType::Audio => cue.downcast_mut::<AudioCue>().map(|cue| cue as &mut dyn ClockMediaCue),
        CueType::Video => cue.downcast_mut::<VideoCue>().map(|cue| cue as &mut dyn ClockMediaCue),
        _ => None,
    }
}

pub(crate) enum ClockCue<'a> {
    Wait(&'a mut WaitCue),
    Media(&'a mut dyn ClockMediaCue),
}

impl<'a> ClockCue<'a> {
    pub(crate) fn supports(kind: CueType) -> bool {
        matches!(kind, CueType::Wait | CueType::Audio | CueType::Video)
    }

    pub(crate) fn from_cue_mut(cue: &'a mut dyn Cue) -> Option<Self> {
        if cue.cue_type() == CueType::Wait {
            WaitCue::from_cue_mut(cue).map(Self::Wait)
        } else {
            media_cue(cue).map(Self::Media)
        }
    }

    /// A marked start cannot take over an existing run. A prepared Load
    /// voice has yet to start and is eligible for the normal marked GO.
    pub(crate) fn next_generation(&self) -> Option<u64> {
        let (cue, prepared): (&dyn Cue, bool) = match self {
            Self::Wait(cue) => (&**cue, false),
            Self::Media(cue) => (&**cue, cue.has_prepared_media()),
        };
        if cue.is_running() || (cue.is_paused() && !prepared) {
            return None;
        }
        Some(cue.play_generation().wrapping_add(1))
    }

    pub(crate) fn follow_clock(self, list: Uuid, generation: u64) {
        match self {
            Self::Wait(cue) => cue.follow_clock(list, generation),
            Self::Media(cue) if cue.is_running() && cue.play_generation() == generation => {
                *cue.clock_run() = Some((list, ClockRun::Controlled));
            }
            Self::Media(_) => {}
        }
    }

    pub(crate) fn detach(self) {
        match self {
            Self::Wait(cue) => cue.detach_clock(),
            Self::Media(cue) => {
                let run = cue.clock_run();
                if matches!(run, Some((_, ClockRun::PausedByClock { .. } | ClockRun::PausedByOperator))) {
                    ClockRun::leave_paused(run);
                } else {
                    *run = None;
                }
            }
        }
    }

    /// The caller owns the audio-device hold set for this desk tick. A clock
    /// return can release clock ownership of a pause while the device is lost.
    pub(crate) fn apply_edge(
        self, list: Uuid, edge: ClockEdge, policy: TcOnStop, context: &CueContext,
        device_frozen: bool, device_holds: &mut std::collections::HashSet<Uuid>,
    ) -> Result<Option<CueState>> {
        let cue = match self {
            Self::Wait(cue) => return cue.apply_clock_edge(list, edge, policy, context),
            Self::Media(cue) => cue,
        };
        let Some((owner, run)) = *cue.clock_run() else { return Ok(None) };
        let before = cue.state();
        if owner != list {
            return Ok(None);
        }
        match (edge, policy, run, before) {
            (ClockEdge::Lost(at), TcOnStop::Pause, ClockRun::Controlled, CueState::Running | CueState::Paused) => {
                if before == CueState::Running {
                    cue.pause(context)?;
                }
                if cue.is_paused() {
                    *cue.clock_run() = Some((list, ClockRun::PausedByClock { at }));
                }
            }
            (ClockEdge::Lost(_), TcOnStop::Stop, ClockRun::Controlled, CueState::Running | CueState::Paused) => {
                cue.stop(context)?;
                device_holds.remove(&cue.id());
            }
            (ClockEdge::Returned(_), _, ClockRun::PausedByClock { .. }, CueState::Paused) => {
                let generation = cue.play_generation();
                if device_frozen && cue.playing_voice_id().is_some() && !cue.uses_silent_speed_clock() {
                    device_holds.insert(cue.id());
                } else {
                    resume_synced_media(cue, context)?;
                    device_holds.remove(&cue.id());
                }
                if cue.play_generation() == generation && (cue.is_running() || cue.is_paused()) {
                    *cue.clock_run() = Some((list, ClockRun::Controlled));
                }
            }
            _ => {}
        }
        Ok((cue.state() != before).then_some(cue.state()))
    }
}

fn resume_synced_media(cue: &mut dyn Cue, context: &CueContext) -> Result<()> {
    // The picture and audio clocks can diverge before an automatic hold
    // takes effect. Resume the paired voice from the actual picture position.
    if cue.cue_type() == CueType::Video {
        if let Some(voice) = cue.playing_voice_id() {
            context.output_engine.resync_audio_to_video(voice);
        }
    }
    cue.resume(context)
}

/// Device pauses and resumes are automatic. Keep the run's clock owner, and
/// leave clock-paused content held even if the audio device has recovered.
pub(crate) fn transition_device_hold(cue: &mut dyn Cue, pause: bool, context: &CueContext) -> Result<bool> {
    let transition = |cue: &mut dyn Cue| if pause { cue.pause(context) } else { resume_synced_media(cue, context) };
    if let Some(media) = media_cue(cue) {
        let owner = *media.clock_run();
        if !pause && matches!(owner, Some((_, ClockRun::PausedByClock { .. } | ClockRun::PausedByOperator))) {
            return Ok(false);
        }
        let generation = media.play_generation();
        let result = transition(media);
        if media.play_generation() == generation && (media.is_running() || media.is_paused()) {
            *media.clock_run() = owner;
        }
        result?;
    } else {
        transition(cue)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freewheel_is_measured_per_list_from_the_last_received_frame() {
        let now = Instant::now();
        let mut short = ClockLock::default();
        let mut long = ClockLock::default();
        for clock in [&mut short, &mut long] {
            assert_eq!(clock.receive(now, TcRate::Fps25, 2), None);
        }
        assert_eq!(short.expire(now + Duration::from_millis(199), 200), None);
        assert_eq!(
            short.expire(now + Duration::from_millis(200), 200),
            Some(ClockEdge::Lost(now + Duration::from_millis(200)))
        );
        assert_eq!(long.expire(now + Duration::from_millis(200), 1200), None);
        assert_eq!(short.expire(now + Duration::from_millis(1200), 200), None);
        assert_eq!(
            long.expire(now + Duration::from_millis(1200), 1200),
            Some(ClockEdge::Lost(now + Duration::from_millis(1200)))
        );
        assert_eq!(
            short.receive(now + Duration::from_millis(1250), TcRate::Fps25, 2),
            Some(ClockEdge::Returned(now + Duration::from_millis(1250)))
        );
        assert_eq!(
            short.receive(now + Duration::from_millis(1330), TcRate::Fps25, 2),
            None
        );
    }

    #[test]
    fn locate_neither_loses_nor_restores_lock_and_zero_freewheel_allows_packet_gap() {
        let now = Instant::now();
        let mut clock = ClockLock::default();
        clock.receive(now, TcRate::Fps25, 2);
        assert_eq!(clock.expire(now + Duration::from_millis(159), 0), None);
        clock.locate();
        let later = now + Duration::from_secs(3);
        assert_eq!(clock.expire(later, 0), None);
        assert_eq!(clock.receive(later, TcRate::Fps25, 1), None);
        assert_eq!(clock.expire(later + Duration::from_millis(79), 0), None);
        assert_eq!(
            clock.expire(later + Duration::from_millis(80), 0),
            Some(ClockEdge::Lost(later + Duration::from_millis(80)))
        );
        clock.locate();
        assert_eq!(clock.expire(later + Duration::from_secs(3), 0), None);
        assert_eq!(
            clock.receive(later + Duration::from_secs(4), TcRate::Fps25, 1),
            Some(ClockEdge::Returned(later + Duration::from_secs(4)))
        );
    }
}
