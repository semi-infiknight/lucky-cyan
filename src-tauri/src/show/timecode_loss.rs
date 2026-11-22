//! Lock-loss decisions for a list's received clock, separate from frame triggers.

use std::time::{Duration, Instant};

use crate::engine::timecode_types::TcRate;

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
