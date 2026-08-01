use serde::{Deserialize, Serialize};

use crate::model::{FrameNumber, Timebase};

pub type ClockTick = u128;

const CLOCK_UNITS_PER_BASE_UNIT: u128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameClockDirection {
    Forward,
    Reverse,
    Still,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameClockRate {
    pub rate_num: i32,
    pub rate_den: u32,
}

impl FrameClockRate {
    pub fn new(rate_num: i32, rate_den: u32) -> Result<Self, String> {
        if rate_den == 0 {
            return Err("rate_den must be greater than zero".to_string());
        }
        Ok(Self { rate_num, rate_den })
    }

    pub fn normal() -> Self {
        Self {
            rate_num: 1,
            rate_den: 1,
        }
    }

    pub fn direction(self) -> FrameClockDirection {
        match self.rate_num.cmp(&0) {
            std::cmp::Ordering::Greater => FrameClockDirection::Forward,
            std::cmp::Ordering::Less => FrameClockDirection::Reverse,
            std::cmp::Ordering::Equal => FrameClockDirection::Still,
        }
    }

    fn magnitude_num(self) -> u128 {
        u128::from(self.rate_num.unsigned_abs())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameClockConfig {
    pub timebase: Timebase,
    pub rate: FrameClockRate,
}

impl FrameClockConfig {
    pub fn new(timebase: Timebase, rate: FrameClockRate) -> Self {
        Self { timebase, rate }
    }

    pub fn frame_interval_ticks(self) -> Option<ClockTick> {
        if self.rate.rate_num == 0 {
            return None;
        }
        let numerator = CLOCK_UNITS_PER_BASE_UNIT
            .checked_mul(u128::from(self.timebase.frame_rate_den))?
            .checked_mul(u128::from(self.rate.rate_den))?;
        let denominator =
            u128::from(self.timebase.frame_rate_num).checked_mul(self.rate.magnitude_num())?;
        ceil_div(numerator, denominator)
    }

    fn due_slots_at(self, elapsed_ticks: ClockTick) -> ClockTick {
        if self.rate.rate_num == 0 {
            return 0;
        }
        let numerator = elapsed_ticks
            .saturating_mul(u128::from(self.timebase.frame_rate_num))
            .saturating_mul(self.rate.magnitude_num());
        let denominator = CLOCK_UNITS_PER_BASE_UNIT
            .saturating_mul(u128::from(self.timebase.frame_rate_den))
            .saturating_mul(u128::from(self.rate.rate_den));
        if denominator == 0 {
            return 0;
        }
        numerator / denominator + 1
    }
}

fn ceil_div(numerator: ClockTick, denominator: ClockTick) -> Option<ClockTick> {
    if denominator == 0 {
        return None;
    }
    numerator
        .checked_add(denominator.saturating_sub(1))?
        .checked_div(denominator)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledFrame {
    pub frame: FrameNumber,
    pub due_slot: ClockTick,
    pub direction: FrameClockDirection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameClock {
    config: FrameClockConfig,
    anchor_frame: FrameNumber,
    anchor_tick: ClockTick,
    delivered_slots: ClockTick,
    running: bool,
}

impl FrameClock {
    pub fn stopped(config: FrameClockConfig) -> Self {
        Self {
            config,
            anchor_frame: 0,
            anchor_tick: 0,
            delivered_slots: 0,
            running: false,
        }
    }

    pub fn start(
        config: FrameClockConfig,
        anchor_frame: FrameNumber,
        anchor_tick: ClockTick,
    ) -> Self {
        Self {
            config,
            anchor_frame,
            anchor_tick,
            delivered_slots: 0,
            running: true,
        }
    }

    pub fn stop(&mut self) {
        self.running = false;
        self.delivered_slots = 0;
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn config(&self) -> FrameClockConfig {
        self.config
    }

    pub fn restart(
        &mut self,
        config: FrameClockConfig,
        anchor_frame: FrameNumber,
        anchor_tick: ClockTick,
    ) {
        *self = Self::start(config, anchor_frame, anchor_tick);
    }

    pub fn next_due_frame(&mut self, now_tick: ClockTick) -> Option<ScheduledFrame> {
        if !self.running || now_tick < self.anchor_tick {
            return None;
        }
        let due_slots = self.config.due_slots_at(now_tick - self.anchor_tick);
        if due_slots <= self.delivered_slots {
            return None;
        }
        let next_slot = self.delivered_slots + 1;
        let frame = self.frame_for_slot(next_slot)?;
        self.delivered_slots = next_slot;
        Some(ScheduledFrame {
            frame,
            due_slot: next_slot,
            direction: self.config.rate.direction(),
        })
    }

    pub fn drain_due_frames(
        &mut self,
        now_tick: ClockTick,
        max_frames: usize,
    ) -> Vec<ScheduledFrame> {
        let mut frames = Vec::new();
        for _ in 0..max_frames {
            let Some(frame) = self.next_due_frame(now_tick) else {
                break;
            };
            frames.push(frame);
        }
        frames
    }

    fn frame_for_slot(&self, slot: ClockTick) -> Option<FrameNumber> {
        let offset = slot.saturating_sub(1);
        match self.config.rate.direction() {
            FrameClockDirection::Forward => {
                let offset = u64::try_from(offset).ok()?;
                self.anchor_frame.checked_add(offset)
            }
            FrameClockDirection::Reverse => {
                let offset = u64::try_from(offset).ok()?;
                self.anchor_frame.checked_sub(offset)
            }
            FrameClockDirection::Still => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_25_frame_timebase_due_frames_are_exact() {
        let mut clock = FrameClock::start(normal_config(25), 100, 1_000);

        assert_eq!(clock.next_due_frame(999), None);
        assert_eq!(clock.next_due_frame(1_000).unwrap().frame, 100);
        assert_eq!(clock.next_due_frame(1_000), None);
        assert_eq!(clock.next_due_frame(40_001_000).unwrap().frame, 101);
        assert_eq!(clock.next_due_frame(80_001_000).unwrap().frame, 102);
    }

    #[test]
    fn normal_50_frame_timebase_uses_half_interval() {
        let mut clock = FrameClock::start(normal_config(50), 10, 0);

        assert_eq!(clock.config().frame_interval_ticks(), Some(20_000_000));
        assert_eq!(clock.next_due_frame(0).unwrap().frame, 10);
        assert_eq!(clock.next_due_frame(19_999_999), None);
        assert_eq!(clock.next_due_frame(20_000_000).unwrap().frame, 11);
    }

    #[test]
    fn normal_30_frame_timebase_never_outputs_frame_early() {
        let mut clock = FrameClock::start(normal_config(30), 10, 0);

        assert_eq!(clock.config().frame_interval_ticks(), Some(33_333_334));
        assert_eq!(clock.next_due_frame(0).unwrap().frame, 10);
        assert_eq!(clock.next_due_frame(33_333_333), None);
        assert_eq!(clock.next_due_frame(33_333_334).unwrap().frame, 11);
        assert_eq!(clock.next_due_frame(66_666_666), None);
        assert_eq!(clock.next_due_frame(66_666_667).unwrap().frame, 12);
    }

    #[test]
    fn normal_60_frame_timebase_never_outputs_frame_early() {
        let mut clock = FrameClock::start(normal_config(60), 20, 0);

        assert_eq!(clock.config().frame_interval_ticks(), Some(16_666_667));
        assert_eq!(clock.next_due_frame(0).unwrap().frame, 20);
        assert_eq!(clock.next_due_frame(16_666_666), None);
        assert_eq!(clock.next_due_frame(16_666_667).unwrap().frame, 21);
        assert_eq!(clock.next_due_frame(33_333_333), None);
        assert_eq!(clock.next_due_frame(33_333_334).unwrap().frame, 22);
    }

    #[test]
    fn rate_two_outputs_two_frames_per_base_interval() {
        let config = FrameClockConfig::new(
            Timebase::new(25, 1).unwrap(),
            FrameClockRate::new(2, 1).unwrap(),
        );
        let mut clock = FrameClock::start(config, 5, 0);

        assert_eq!(clock.config().frame_interval_ticks(), Some(20_000_000));
        assert_eq!(clock.next_due_frame(0).unwrap().frame, 5);
        assert_eq!(clock.next_due_frame(20_000_000).unwrap().frame, 6);
        assert_eq!(clock.next_due_frame(40_000_000).unwrap().frame, 7);
    }

    #[test]
    fn fractional_rate_outputs_frame_after_scaled_interval() {
        let config = FrameClockConfig::new(
            Timebase::new(25, 1).unwrap(),
            FrameClockRate::new(1, 2).unwrap(),
        );
        let mut clock = FrameClock::start(config, 5, 0);

        assert_eq!(clock.config().frame_interval_ticks(), Some(80_000_000));
        assert_eq!(clock.next_due_frame(0).unwrap().frame, 5);
        assert_eq!(clock.next_due_frame(79_999_999), None);
        assert_eq!(clock.next_due_frame(80_000_000).unwrap().frame, 6);
    }

    #[test]
    fn reverse_rate_counts_down_by_frame() {
        let config = FrameClockConfig::new(
            Timebase::new(25, 1).unwrap(),
            FrameClockRate::new(-1, 1).unwrap(),
        );
        let mut clock = FrameClock::start(config, 3, 0);

        assert_eq!(clock.next_due_frame(0).unwrap().frame, 3);
        assert_eq!(clock.next_due_frame(40_000_000).unwrap().frame, 2);
        assert_eq!(clock.next_due_frame(80_000_000).unwrap().frame, 1);
        assert_eq!(clock.next_due_frame(120_000_000).unwrap().frame, 0);
        assert_eq!(clock.next_due_frame(160_000_000), None);
    }

    #[test]
    fn drain_due_frames_caps_catchup_work() {
        let mut clock = FrameClock::start(normal_config(25), 0, 0);

        let frames = clock.drain_due_frames(200_000_000, 3);

        assert_eq!(
            frames
                .into_iter()
                .map(|scheduled| scheduled.frame)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(clock.next_due_frame(200_000_000).unwrap().frame, 3);
    }

    #[test]
    fn still_rate_never_outputs_due_frame() {
        let config = FrameClockConfig::new(
            Timebase::new(25, 1).unwrap(),
            FrameClockRate::new(0, 1).unwrap(),
        );
        let mut clock = FrameClock::start(config, 10, 0);

        assert_eq!(clock.config().frame_interval_ticks(), None);
        assert_eq!(clock.next_due_frame(0), None);
        assert_eq!(clock.next_due_frame(1_000_000_000), None);
    }

    #[test]
    fn clock_state_serialization_stays_neutral_and_frame_based() {
        let clock = FrameClock::start(normal_config(25), 10, 0);
        let text = serde_json::to_string(&clock).unwrap().to_ascii_lowercase();
        let value = serde_json::to_value(&clock).unwrap();
        let fields = value.as_object().expect("clock object");

        assert!(text.contains("frame"));
        assert_eq!(fields.len(), 5);
        for field in [
            "config",
            "anchor_frame",
            "anchor_tick",
            "delivered_slots",
            "running",
        ] {
            assert!(fields.contains_key(field), "missing field: {field}");
        }
    }

    fn normal_config(frame_rate_num: u32) -> FrameClockConfig {
        FrameClockConfig::new(
            Timebase::new(frame_rate_num, 1).unwrap(),
            FrameClockRate::normal(),
        )
    }
}
