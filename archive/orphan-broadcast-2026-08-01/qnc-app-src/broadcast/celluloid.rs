//! Celluloid/timecode carrier for QNC broadcast playback.
//!
//! In the QNC/Kodak model the celluloid strip is the **common carrier**
//! (zajednička nosilja): frame identity, source FPS, range, and PTS mapping.
//! It is not a visual layer. Filmstrip may paint below it as an underlay.
//! Every program layer (video, A1–A4 mono audio, markers, effects) registers
//! against this one timecode substrate — no layer owns time.

use super::timebase::{FrameNumber, FrameRange, Timebase};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CelluloidTrack {
    pub project_id: String,
    pub virtual_shot_id: String,
    pub clip_id: String,
    pub timebase: Timebase,
    pub source_range: FrameRange,
}

impl CelluloidTrack {
    pub fn new(
        project_id: impl Into<String>,
        virtual_shot_id: impl Into<String>,
        clip_id: impl Into<String>,
        timebase: Timebase,
        source_range: FrameRange,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            virtual_shot_id: virtual_shot_id.into(),
            clip_id: clip_id.into(),
            timebase,
            source_range,
        }
    }

    pub fn clamp_source_frame(&self, frame: FrameNumber) -> FrameNumber {
        self.source_range.clamp(frame)
    }

    pub fn is_transparent_carrier(&self) -> bool {
        true
    }

    pub fn duration_frames(&self) -> i64 {
        (self.source_range.end_exclusive.0 - self.source_range.start.0).max(1)
    }

    pub fn duration_sec(&self) -> f64 {
        self.timebase
            .seconds_at_frame(FrameNumber(self.duration_frames()))
    }

    pub fn source_frame_at_program_seconds(&self, seconds: f64) -> FrameNumber {
        let offset = self.timebase.frame_floor_at_seconds(seconds).0;
        self.clamp_source_frame(FrameNumber(self.source_range.start.0 + offset))
    }

    pub fn program_seconds_at_source_frame(&self, frame: FrameNumber) -> f64 {
        let source_frame = self.clamp_source_frame(frame);
        self.timebase
            .seconds_at_frame(FrameNumber(source_frame.0 - self.source_range.start.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn celluloid_maps_program_seconds_to_source_frames() {
        let carrier = CelluloidTrack::new(
            "project",
            "shot",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(100), FrameNumber(200)),
        );

        assert_eq!(
            carrier.source_frame_at_program_seconds(0.08),
            FrameNumber(102)
        );
        assert_eq!(
            carrier.program_seconds_at_source_frame(FrameNumber(102)),
            0.08
        );
    }

    #[test]
    fn celluloid_is_the_timecode_range() {
        let carrier = CelluloidTrack::new(
            "project",
            "shot",
            "clip",
            Timebase::from_source_fps(50.0),
            FrameRange::new(FrameNumber(10), FrameNumber(15)),
        );

        assert_eq!(carrier.duration_frames(), 5);
        assert_eq!(carrier.clamp_source_frame(FrameNumber(99)), FrameNumber(14));
    }

    #[test]
    fn celluloid_is_transparent_timecode_carrier() {
        let carrier = CelluloidTrack::new(
            "project",
            "shot",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(25)),
        );

        assert!(carrier.is_transparent_carrier());
    }
}
