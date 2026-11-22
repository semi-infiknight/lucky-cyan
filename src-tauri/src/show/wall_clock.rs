//! Saved local-clock schedules and their in-memory arming state.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use chrono::{DateTime, Datelike, FixedOffset, Local, NaiveDate, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};

use crate::cue::types::CueId;

const MAX_CLOCK_GAP: Duration = Duration::from_millis(500);
const MAX_CLOCK_DRIFT: Duration = Duration::from_millis(100);

/// A time on the host's local wall clock. Bit zero is Monday.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WallClockTrigger {
    pub time: NaiveTime,
    pub weekdays: u8,
}

impl WallClockTrigger {
    pub(crate) fn validate(self) -> anyhow::Result<Self> {
        anyhow::ensure!(
            self.time.nanosecond() == 0,
            "Use whole seconds for the wall clock trigger"
        );
        anyhow::ensure!(
            (1..=127).contains(&self.weekdays),
            "Select at least one weekday"
        );
        Ok(self)
    }

    fn crosses(self, previous: Observation, current: Observation) -> bool {
        let Some(elapsed) = current.monotonic.checked_duration_since(previous.monotonic) else {
            return false;
        };
        let Ok(wall_elapsed) = (current.local - previous.local).to_std() else {
            return false;
        };
        if elapsed > MAX_CLOCK_GAP
            || wall_elapsed > MAX_CLOCK_GAP
            || elapsed.abs_diff(wall_elapsed) > MAX_CLOCK_DRIFT
            || previous.local.offset() != current.local.offset()
        {
            return false;
        }
        let date = current.local.date_naive();
        let weekday = date.weekday().num_days_from_monday();
        let due = date.and_time(self.time);
        self.weekdays & (1 << weekday) != 0
            && previous.local.naive_local() < due
            && due <= current.local.naive_local()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Observation {
    local: DateTime<FixedOffset>,
    monotonic: Instant,
}

impl Observation {
    pub(crate) fn now() -> Self {
        Self {
            local: Local::now().fixed_offset(),
            monotonic: Instant::now(),
        }
    }

    pub(crate) fn is_fresh(self) -> bool {
        self.monotonic.elapsed() <= MAX_CLOCK_GAP
    }
}

#[derive(Default)]
pub(crate) struct WallClockSession {
    armed: BTreeMap<CueId, Observation>,
    attempted: BTreeMap<CueId, (NaiveDate, BTreeSet<NaiveTime>)>,
}

impl WallClockSession {
    pub(crate) fn set_armed(&mut self, id: CueId, armed: bool, now: Observation) {
        if armed {
            self.armed.entry(id).or_insert(now);
        } else {
            self.armed.remove(&id);
        }
    }

    pub(crate) fn is_armed(&self, id: CueId) -> bool {
        self.armed.contains_key(&id)
    }

    pub(crate) fn has_armed(&self) -> bool {
        !self.armed.is_empty()
    }

    /// A stop, undo, or show switch can withdraw authorization without
    /// forgetting the occurrences already attempted in this open workspace.
    pub(crate) fn disarm_all(&mut self) {
        self.armed.clear();
    }

    pub(crate) fn collect(
        &mut self,
        schedules: &BTreeMap<CueId, WallClockTrigger>,
        now: Observation,
    ) -> Vec<CueId> {
        let mut due = Vec::new();
        self.armed.retain(|id, previous| {
            let Some(trigger) = schedules.get(id) else {
                return false;
            };
            let before = std::mem::replace(previous, now);
            let date = now.local.date_naive();
            if trigger.crosses(before, now) {
                let (day, times) = self
                    .attempted
                    .entry(*id)
                    .or_insert_with(|| (date, BTreeSet::new()));
                if *day < date {
                    *day = date;
                    times.clear();
                }
                if *day == date && times.insert(trigger.time) {
                    due.push(*id);
                }
            }
            true
        });
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show_time(time: &str, ms: u64, from: Instant) -> Observation {
        Observation {
            local: DateTime::parse_from_rfc3339(time).unwrap(),
            monotonic: from + Duration::from_millis(ms),
        }
    }

    #[test]
    fn fires_once_across_midnight_on_the_selected_weekday() {
        let id = CueId::new_v4();
        let schedule = BTreeMap::from([(
            id,
            WallClockTrigger {
                time: "00:00:00".parse().unwrap(),
                weekdays: 1,
            },
        )]);
        let mut session = WallClockSession::default();
        let instant = Instant::now();
        session.set_armed(
            id,
            true,
            show_time("2026-11-22T23:59:59.950+00:00", 0, instant),
        );
        assert_eq!(
            session.collect(
                &schedule,
                show_time("2026-11-23T00:00:00.022+00:00", 72, instant)
            ),
            [id]
        );
        assert!(session
            .collect(
                &schedule,
                show_time("2026-11-23T00:00:00.050+00:00", 100, instant)
            )
            .is_empty());
        session.disarm_all();
        session.set_armed(
            id,
            true,
            show_time("2026-11-22T23:59:59.950+00:00", 1_000, instant),
        );
        assert!(session
            .collect(
                &schedule,
                show_time("2026-11-23T00:00:00.020+00:00", 1_070, instant)
            )
            .is_empty());
        assert!(session
            .collect(
                &schedule,
                show_time("2026-11-23T23:59:59.960+00:00", 2_000, instant)
            )
            .is_empty());
        assert!(session
            .collect(
                &schedule,
                show_time("2026-11-24T00:00:00.020+00:00", 2_060, instant)
            )
            .is_empty());
    }

    #[test]
    fn holds_events_when_loading_sleep_and_clock_corrections_skip_the_boundary() {
        let id = CueId::new_v4();
        let schedule = BTreeMap::from([(
            id,
            WallClockTrigger {
                time: "12:00:00".parse().unwrap(),
                weekdays: 127,
            },
        )]);
        let mut session = WallClockSession::default();
        let instant = Instant::now();
        let before = show_time("2026-11-22T11:59:59.800+00:00", 0, instant);
        let late = show_time("2026-11-22T12:00:00.600+00:00", 800, instant);
        assert!(session.collect(&schedule, late).is_empty());
        session.set_armed(id, true, before);
        assert!(session.collect(&schedule, late).is_empty());
        assert!(session
            .collect(
                &schedule,
                show_time("2026-11-22T11:59:59.800+00:00", 840, instant)
            )
            .is_empty());
        assert!(session
            .collect(
                &schedule,
                show_time("2026-11-22T12:00:00.100+00:00", 870, instant)
            )
            .is_empty());
        session.disarm_all();
        session.set_armed(id, true, late);
        assert!(session
            .collect(
                &schedule,
                show_time("2026-11-22T12:00:00.633+00:00", 833, instant)
            )
            .is_empty());
    }

    #[test]
    fn ignores_offset_jumps_and_repeats_only_once_on_the_fall_back_date() {
        let id = CueId::new_v4();
        let schedule = BTreeMap::from([(
            id,
            WallClockTrigger {
                time: "01:15:00".parse().unwrap(),
                weekdays: 127,
            },
        )]);
        let mut session = WallClockSession::default();
        let instant = Instant::now();
        session.set_armed(
            id,
            true,
            show_time("2026-11-01T01:14:59.970-04:00", 0, instant),
        );
        assert_eq!(
            session.collect(
                &schedule,
                show_time("2026-11-01T01:15:00.030-04:00", 60, instant)
            ),
            [id]
        );
        assert!(session
            .collect(
                &schedule,
                show_time("2026-11-01T01:14:59.970-05:00", 900, instant)
            )
            .is_empty());
        assert!(session
            .collect(
                &schedule,
                show_time("2026-11-01T01:15:00.030-05:00", 960, instant)
            )
            .is_empty());

        let skipped = WallClockTrigger {
            time: "02:30:00".parse().unwrap(),
            weekdays: 127,
        };
        assert!(!skipped.crosses(
            show_time("2026-03-08T01:59:59.980-05:00", 1_000, instant),
            show_time("2026-03-08T03:00:00.010-04:00", 1_030, instant),
        ));
    }

    #[test]
    fn a_new_time_can_be_armed_for_the_same_day_without_replaying_an_old_occurrence() {
        let id = CueId::new_v4();
        let mut schedule = BTreeMap::new();
        let mut session = WallClockSession::default();
        let instant = Instant::now();
        for (hour, ms, occurs) in [("12", 0, true), ("13", 100, true), ("12", 200, false)] {
            schedule.insert(
                id,
                WallClockTrigger {
                    time: format!("{hour}:00:01").parse().unwrap(),
                    weekdays: 127,
                },
            );
            session.disarm_all();
            session.set_armed(
                id,
                true,
                show_time(&format!("2026-11-22T{hour}:00:00.970+00:00"), ms, instant),
            );
            let due = session.collect(
                &schedule,
                show_time(
                    &format!("2026-11-22T{hour}:00:01.030+00:00"),
                    ms + 60,
                    instant,
                ),
            );
            assert_eq!(!due.is_empty(), occurs);
        }
    }
}
