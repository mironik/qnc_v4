//! Broadcast frame/audio synchronization helpers.
//!
//! QNC uses the celluloid carrier as the common time bus. Video frame identity
//! and audio sample windows are both derived from that carrier; neither audio
//! hardware nor a video worker owns playback time.

use super::celluloid::CelluloidTrack;
use super::timebase::{FrameNumber, Timebase};

pub const BROADCAST_AUDIO_SAMPLE_RATE_HZ: u32 = 48_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSampleSpan {
    pub start_sample: i64,
    pub end_exclusive: i64,
}

impl AudioSampleSpan {
    pub fn new(start_sample: i64, end_exclusive: i64) -> Self {
        Self {
            start_sample: start_sample.max(0),
            end_exclusive: end_exclusive.max(start_sample + 1),
        }
    }

    pub fn len(self) -> usize {
        (self.end_exclusive - self.start_sample).max(0) as usize
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub fn from_carrier_frame(
        carrier: &CelluloidTrack,
        source_frame: FrameNumber,
        sample_rate_hz: u32,
    ) -> Self {
        let frame = carrier.clamp_source_frame(source_frame);
        let frame_offset = (frame.0 - carrier.source_range.start.0).max(0);
        Self::from_timebase_offsets(
            carrier.timebase,
            frame_offset,
            frame_offset + 1,
            sample_rate_hz,
        )
    }

    pub fn from_timebase_offsets(
        timebase: Timebase,
        start_frame_offset: i64,
        end_frame_offset: i64,
        sample_rate_hz: u32,
    ) -> Self {
        let sample_rate_hz = sample_rate_hz.max(1);
        let start = sample_index_at_frame_offset(timebase, start_frame_offset, sample_rate_hz);
        let end = sample_index_at_frame_offset(
            timebase,
            end_frame_offset.max(start_frame_offset + 1),
            sample_rate_hz,
        );
        Self::new(start, end)
    }
}

pub fn sample_index_at_frame_offset(
    timebase: Timebase,
    frame_offset: i64,
    sample_rate_hz: u32,
) -> i64 {
    let frame_offset = frame_offset.max(0) as i128;
    let den = timebase.den as i128;
    let num = timebase.num.max(1) as i128;
    let sample_rate = sample_rate_hz.max(1) as i128;
    let numerator = frame_offset * den * sample_rate;
    ((numerator * 2 + num) / (num * 2)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::timebase::FrameRange;

    #[test]
    fn audio_span_uses_carrier_timebase_for_48khz_boundaries() {
        let carrier = CelluloidTrack::new(
            "project",
            "shot",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(100), FrameNumber(200)),
        );

        assert_eq!(
            AudioSampleSpan::from_carrier_frame(
                &carrier,
                FrameNumber(100),
                BROADCAST_AUDIO_SAMPLE_RATE_HZ
            ),
            AudioSampleSpan::new(0, 1_920)
        );
        assert_eq!(
            AudioSampleSpan::from_carrier_frame(
                &carrier,
                FrameNumber(101),
                BROADCAST_AUDIO_SAMPLE_RATE_HZ
            ),
            AudioSampleSpan::new(1_920, 3_840)
        );
    }

    #[test]
    fn audio_span_preserves_fractional_broadcast_rates() {
        let timebase = Timebase::from_source_fps(29.97);

        let first =
            AudioSampleSpan::from_timebase_offsets(timebase, 0, 1, BROADCAST_AUDIO_SAMPLE_RATE_HZ);
        let second =
            AudioSampleSpan::from_timebase_offsets(timebase, 1, 2, BROADCAST_AUDIO_SAMPLE_RATE_HZ);
        let five =
            AudioSampleSpan::from_timebase_offsets(timebase, 0, 5, BROADCAST_AUDIO_SAMPLE_RATE_HZ);

        assert_eq!(first, AudioSampleSpan::new(0, 1_602));
        assert_eq!(second, AudioSampleSpan::new(1_602, 3_203));
        assert_eq!(five, AudioSampleSpan::new(0, 8_008));
    }

    #[test]
    fn audio_span_is_clamped_to_carrier_range() {
        let carrier = CelluloidTrack::new(
            "project",
            "shot",
            "clip",
            Timebase::from_source_fps(25.0),
            crate::broadcast::timebase::FrameRange::new(FrameNumber(100), FrameNumber(102)),
        );

        assert_eq!(
            AudioSampleSpan::from_carrier_frame(
                &carrier,
                FrameNumber(999),
                BROADCAST_AUDIO_SAMPLE_RATE_HZ
            ),
            AudioSampleSpan::new(1_920, 3_840)
        );
    }
}
