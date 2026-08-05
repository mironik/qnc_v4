//! Neutral progress-bar adapter for `QncTimeline`.
//!
//! This module owns only frame/paint projection. It has no knowledge of forms,
//! workflow, database, player runtime, or source catalogs.

use eframe::egui;

use crate::frame_time::{frame_to_seconds, normalize_fps, seconds_to_frame};
use crate::qnc_timeline::{
    ExpandedAudio, LayerFlags, QncTimeline, TimelineCoverSpan, TimelineFocusPaint,
    TimelineMarkerPin, TimelineSlotSpan,
};

#[derive(Debug, Clone, Copy)]
pub struct TimelineProgressModel {
    fps: f64,
    duration_frames: i64,
    playhead_frame: i64,
    in_frame: i64,
    out_frame: i64,
}

impl TimelineProgressModel {
    pub fn from_seconds(
        fps: f64,
        duration_sec: f64,
        playhead_sec: f64,
        in_sec: f64,
        out_sec: f64,
    ) -> Self {
        let fps = normalize_fps(fps);
        let duration_frames = seconds_to_frame(duration_sec.max(0.0), fps).max(1);
        let clamp = |frame: i64| frame.clamp(0, duration_frames);
        Self {
            fps,
            duration_frames,
            playhead_frame: clamp(seconds_to_frame(playhead_sec, fps)),
            in_frame: clamp(seconds_to_frame(in_sec, fps)),
            out_frame: clamp(seconds_to_frame(out_sec, fps)),
        }
    }

    /// Carrier/playhead authority — use in [`crate::carrier_sync`], not form seconds.
    pub fn from_carrier(
        fps: f64,
        duration_frames: i64,
        playhead_frame: i64,
        in_frame: i64,
        out_frame: i64,
    ) -> Self {
        let fps = normalize_fps(fps);
        let duration_frames = duration_frames.max(1);
        let clamp = |frame: i64| frame.clamp(0, duration_frames);
        Self {
            fps,
            duration_frames,
            playhead_frame: clamp(playhead_frame),
            in_frame: clamp(in_frame),
            out_frame: clamp(out_frame.max(in_frame)),
        }
    }

    pub fn duration_frames(self) -> i64 {
        self.duration_frames
    }

    pub fn playhead_frame(self) -> i64 {
        self.playhead_frame
    }

    pub fn duration_sec(self) -> f64 {
        frame_to_seconds(self.duration_frames, self.fps).max(0.04)
    }

    pub fn playhead_sec(self) -> f64 {
        frame_to_seconds(self.playhead_frame, self.fps)
    }

    pub fn in_sec(self) -> f64 {
        frame_to_seconds(self.in_frame, self.fps)
    }

    pub fn out_sec(self) -> f64 {
        frame_to_seconds(self.out_frame, self.fps)
    }

    pub fn frame_at_seconds(self, seconds: f64) -> i64 {
        seconds_to_frame(seconds, self.fps).clamp(0, self.duration_frames)
    }

    pub fn seconds_at_frame(self, frame: i64) -> f64 {
        frame_to_seconds(frame.clamp(0, self.duration_frames), self.fps)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineProgressIntent {
    None,
    CueFrame(i64),
    ToggleAudioExpand(ExpandedAudio),
}

pub struct TimelineProgressInput<'a> {
    pub model: TimelineProgressModel,
    pub layers: LayerFlags,
    pub video_background: Option<&'a dyn Fn(&mut egui::Ui, egui::Rect)>,
    pub focus: TimelineFocusPaint,
    pub expanded_audio: ExpandedAudio,
    pub a1_peaks: &'a [f32],
    pub a2_peaks: &'a [f32],
    pub a3_peaks: &'a [f32],
    pub a4_peaks: &'a [f32],
    pub covers: &'a [TimelineCoverSpan<'a>],
    pub marker_slots: &'a [TimelineSlotSpan<'a>],
    pub markers: &'a [TimelineMarkerPin],
    pub base_video_blank: bool,
}

pub fn show(ui: &mut egui::Ui, input: TimelineProgressInput<'_>) -> TimelineProgressIntent {
    let interact = QncTimeline {
        layers: input.layers,
        duration_sec: input.model.duration_sec(),
        playhead_sec: input.model.playhead_sec(),
        source_in: input.model.in_sec(),
        source_out: input.model.out_sec(),
        video_background: input.video_background,
        focus: input.focus,
        expanded_audio: input.expanded_audio,
        a1_peaks: input.a1_peaks,
        a2_peaks: input.a2_peaks,
        a3_peaks: input.a3_peaks,
        a4_peaks: input.a4_peaks,
        covers: input.covers,
        marker_slots: input.marker_slots,
        markers: input.markers,
        base_video_blank: input.base_video_blank,
    }
    .show(ui);

    if let Some(lane) = interact.expand_click {
        TimelineProgressIntent::ToggleAudioExpand(lane)
    } else if let Some(sec) = interact.seek_sec {
        TimelineProgressIntent::CueFrame(input.model.frame_at_seconds(sec))
    } else {
        TimelineProgressIntent::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_model_is_frame_based_and_clamped() {
        let model = TimelineProgressModel::from_seconds(25.0, 4.0, 2.04, -1.0, 9.0);

        assert_eq!(model.duration_frames(), 100);
        assert_eq!(model.playhead_frame(), 51);
        assert_eq!(model.in_sec(), 0.0);
        assert_eq!(model.out_sec(), 4.0);
    }

    #[test]
    fn progress_seek_returns_frame_number() {
        let model = TimelineProgressModel::from_seconds(50.0, 10.0, 0.0, 0.0, 10.0);

        assert_eq!(model.frame_at_seconds(1.5), 75);
        assert_eq!(model.seconds_at_frame(75), 1.5);
    }

    #[test]
    fn from_carrier_uses_frame_authority_not_seconds() {
        let model = TimelineProgressModel::from_carrier(25.0, 250, 100, 10, 200);

        assert_eq!(model.playhead_frame(), 100);
        assert_eq!(model.duration_frames(), 250);
        assert_eq!(model.in_sec(), 0.4);
        assert_eq!(model.out_sec(), 8.0);
    }

    #[test]
    fn from_carrier_clamps_playhead_and_marks() {
        let model = TimelineProgressModel::from_carrier(25.0, 100, 999, -5, 50);

        assert_eq!(model.playhead_frame(), 100);
        assert_eq!(model.in_sec(), 0.0);
        assert_eq!(model.out_sec(), 2.0);
    }
}
