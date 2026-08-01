//! Broadcast program clock.
//!
//! The playback clock is not owned by the audio sink and not owned by the video
//! worker. It is the program/reference clock for the current virtual shot. Audio
//! and video renderers are consumers of this clock. A source clip without audio
//! still plays deterministically from source FPS / PTS.

use std::time::{Duration, Instant};

use super::timebase::{FrameNumber, FrameRange, Timebase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockState {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockReference {
    /// Desktop fallback: high-resolution monotonic clock.
    ///
    /// This is the native-app equivalent of a local reference clock. It is not
    /// an audio sink and not a decoded video frame queue.
    InternalMonotonic,
    /// Future production path: hardware / SDI / house-sync reference.
    ExternalVideoReference,
}

#[derive(Debug, Clone)]
pub struct BroadcastMasterClock {
    timebase: Timebase,
    range: FrameRange,
    reference: ClockReference,
    state: ClockState,
    anchor_frame: FrameNumber,
    anchor_instant: Option<Instant>,
}

impl BroadcastMasterClock {
    pub fn new(timebase: Timebase, range: FrameRange, reference: ClockReference) -> Self {
        Self {
            timebase,
            range,
            reference,
            state: ClockState::Stopped,
            anchor_frame: range.start,
            anchor_instant: None,
        }
    }

    pub fn play_from(&mut self, frame: FrameNumber, now: Instant) {
        self.anchor_frame = self.range.clamp(frame);
        self.anchor_instant = Some(now);
        self.state = ClockState::Playing;
    }

    pub fn pause(&mut self, now: Instant) {
        self.anchor_frame = self.current_frame(now);
        self.anchor_instant = None;
        self.state = ClockState::Paused;
    }

    pub fn stop(&mut self) {
        self.anchor_frame = self.range.start;
        self.anchor_instant = None;
        self.state = ClockState::Stopped;
    }

    pub fn seek(&mut self, frame: FrameNumber, now: Instant) {
        self.anchor_frame = self.range.clamp(frame);
        if self.state == ClockState::Playing {
            self.anchor_instant = Some(now);
        }
    }

    /// Freeze the program clock at `frame` while staying Playing.
    ///
    /// Used when decode lags the wall clock: never skip frames (that jumps
    /// picture and chops PCM into unintelligible audio). Resume with
    /// [`Self::resume`] once the next decoded frame is ready.
    pub fn stall_at(&mut self, frame: FrameNumber) {
        if self.state != ClockState::Playing {
            return;
        }
        self.anchor_frame = self.range.clamp(frame);
        self.anchor_instant = None;
    }

    /// Continue after [`Self::stall_at`] from the frozen frame in realtime.
    pub fn resume(&mut self, now: Instant) {
        if self.state == ClockState::Playing && self.anchor_instant.is_none() {
            self.anchor_instant = Some(now);
        }
    }

    pub fn is_stalled(&self) -> bool {
        self.state == ClockState::Playing && self.anchor_instant.is_none()
    }

    pub fn state(&self) -> ClockState {
        self.state
    }

    pub fn reference(&self) -> ClockReference {
        self.reference
    }

    pub fn current_frame(&self, now: Instant) -> FrameNumber {
        if self.state != ClockState::Playing {
            return self.anchor_frame;
        }
        let Some(anchor) = self.anchor_instant else {
            // Stalled (or play without anchor): hold frame, do not free-run.
            return self.anchor_frame;
        };
        let elapsed = now.saturating_duration_since(anchor).as_secs_f64();
        let delta = self.timebase.frame_floor_at_seconds(elapsed).0;
        self.range.clamp(FrameNumber(self.anchor_frame.0 + delta))
    }

    pub fn current_position_sec(&self, now: Instant) -> f64 {
        let frame = self.current_frame(now);
        self.timebase
            .seconds_at_frame(FrameNumber(frame.0 - self.range.start.0))
    }

    pub fn next_frame_deadline(&self, now: Instant) -> Option<Duration> {
        if self.state != ClockState::Playing {
            return None;
        }
        let anchor = self.anchor_instant?;
        let current = self.current_frame(now);
        if current.0 >= self.range.end_exclusive.0 - 1 {
            return None;
        }
        let next_delta = FrameNumber(current.0 + 1 - self.anchor_frame.0);
        let deadline = anchor + Duration::from_secs_f64(self.timebase.seconds_at_frame(next_delta));
        Some(deadline.saturating_duration_since(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_clock_runs_without_audio_track() {
        let tb = Timebase::from_source_fps(50.0);
        let range = FrameRange::new(FrameNumber(100), FrameNumber(200));
        let mut clock = BroadcastMasterClock::new(tb, range, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        clock.play_from(FrameNumber(100), t0);

        assert_eq!(
            clock.current_frame(t0 + Duration::from_millis(40)),
            FrameNumber(102)
        );
    }

    #[test]
    fn program_clock_is_not_audio_sink() {
        let tb = Timebase::from_source_fps(25.0);
        let range = FrameRange::new(FrameNumber(0), FrameNumber(100));
        let clock = BroadcastMasterClock::new(tb, range, ClockReference::InternalMonotonic);

        assert_eq!(clock.reference(), ClockReference::InternalMonotonic);
    }

    #[test]
    fn program_clock_clamps_to_virtual_shot_range() {
        let tb = Timebase::from_source_fps(25.0);
        let range = FrameRange::new(FrameNumber(10), FrameNumber(12));
        let mut clock = BroadcastMasterClock::new(tb, range, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        clock.play_from(FrameNumber(10), t0);

        assert_eq!(
            clock.current_frame(t0 + Duration::from_secs(10)),
            FrameNumber(11)
        );
    }

    #[test]
    fn program_clock_stall_holds_frame_until_resume() {
        let tb = Timebase::from_source_fps(25.0);
        let range = FrameRange::new(FrameNumber(0), FrameNumber(100));
        let mut clock = BroadcastMasterClock::new(tb, range, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        clock.play_from(FrameNumber(10), t0);
        clock.stall_at(FrameNumber(12));

        assert!(clock.is_stalled());
        assert_eq!(
            clock.current_frame(t0 + Duration::from_secs(2)),
            FrameNumber(12)
        );

        let t1 = t0 + Duration::from_secs(2);
        clock.resume(t1);
        assert!(!clock.is_stalled());
        assert_eq!(
            clock.current_frame(t1 + Duration::from_millis(40)),
            FrameNumber(13)
        );
    }
}
