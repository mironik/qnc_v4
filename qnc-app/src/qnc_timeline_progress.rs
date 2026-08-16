//! Neutral progress-bar adapter for `QncTimeline`.
//!
//! This module owns only frame/paint projection. It has no knowledge of forms,
//! workflow, database, player runtime, or source catalogs.

use eframe::egui;

use crate::qnc_timeline::{
    ExpandedAudio, LayerFlags, QncTimeline, TimelineCoverSpan, TimelineFocusPaint,
    TimelineMarkerPin, TimelineSlotSpan,
};

#[derive(Debug, Clone, Copy)]
pub struct TimelineProgressModel {
    duration_frames: i64,
    playhead_frame: i64,
    shot_in_frame: i64,
    shot_out_frame: i64,
    draft_in_frame: i64,
    draft_out_frame: i64,
}

impl TimelineProgressModel {
    /// Carrier/playhead authority — use in [`crate::carrier_sync`], not form seconds.
    #[allow(dead_code)]
    pub fn from_carrier(
        _fps: f64,
        duration_frames: i64,
        playhead_frame: i64,
        in_frame: i64,
        out_frame: i64,
    ) -> Self {
        Self::from_ranges(
            _fps,
            duration_frames,
            playhead_frame,
            in_frame,
            out_frame,
            in_frame,
            out_frame,
        )
    }

    pub fn from_ranges(
        _fps: f64,
        duration_frames: i64,
        playhead_frame: i64,
        shot_in_frame: i64,
        shot_out_frame: i64,
        draft_in_frame: i64,
        draft_out_frame: i64,
    ) -> Self {
        let duration_frames = duration_frames.max(1);
        let clamp = |frame: i64| frame.clamp(0, duration_frames);
        Self {
            duration_frames,
            playhead_frame: clamp(playhead_frame),
            shot_in_frame: clamp(shot_in_frame),
            shot_out_frame: clamp(shot_out_frame.max(shot_in_frame)),
            draft_in_frame: clamp(draft_in_frame),
            draft_out_frame: clamp(draft_out_frame.max(draft_in_frame)),
        }
    }

    pub fn duration_frames(self) -> i64 {
        self.duration_frames
    }

    pub fn playhead_frame(self) -> i64 {
        self.playhead_frame
    }

    pub fn shot_in_frame(self) -> i64 {
        self.shot_in_frame
    }

    pub fn shot_out_frame(self) -> i64 {
        self.shot_out_frame
    }

    pub fn draft_in_frame(self) -> i64 {
        self.draft_in_frame
    }

    pub fn draft_out_frame(self) -> i64 {
        self.draft_out_frame
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
        duration_frames: input.model.duration_frames(),
        playhead_frame: input.model.playhead_frame(),
        shot_in_frame: input.model.shot_in_frame(),
        shot_out_frame: input.model.shot_out_frame(),
        draft_in_frame: input.model.draft_in_frame(),
        draft_out_frame: input.model.draft_out_frame(),
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
    } else if let Some(frame) = interact.seek_frame {
        TimelineProgressIntent::CueFrame(frame)
    } else {
        TimelineProgressIntent::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_model_is_frame_based_and_clamped() {
        let model = TimelineProgressModel::from_carrier(25.0, 100, 51, -1, 999);

        assert_eq!(model.duration_frames(), 100);
        assert_eq!(model.playhead_frame(), 51);
        assert_eq!(model.shot_in_frame(), 0);
        assert_eq!(model.shot_out_frame(), 100);
        assert_eq!(model.draft_in_frame(), 0);
        assert_eq!(model.draft_out_frame(), 100);
    }

    #[test]
    fn from_carrier_keeps_mark_range_in_frame_space() {
        let model = TimelineProgressModel::from_carrier(25.0, 100, 0, 75, 50);

        assert_eq!(model.shot_in_frame(), 75);
        assert_eq!(model.shot_out_frame(), 75);
        assert_eq!(model.draft_in_frame(), 75);
        assert_eq!(model.draft_out_frame(), 75);
    }

    #[test]
    fn from_carrier_uses_frame_authority_not_seconds() {
        let model = TimelineProgressModel::from_carrier(25.0, 250, 100, 10, 200);

        assert_eq!(model.playhead_frame(), 100);
        assert_eq!(model.duration_frames(), 250);
        assert_eq!(model.shot_in_frame(), 10);
        assert_eq!(model.shot_out_frame(), 200);
        assert_eq!(model.draft_in_frame(), 10);
        assert_eq!(model.draft_out_frame(), 200);
    }

    #[test]
    fn from_carrier_clamps_playhead_and_marks() {
        let model = TimelineProgressModel::from_carrier(25.0, 100, 999, -5, 50);

        assert_eq!(model.playhead_frame(), 100);
        assert_eq!(model.shot_in_frame(), 0);
        assert_eq!(model.shot_out_frame(), 50);
        assert_eq!(model.draft_in_frame(), 0);
        assert_eq!(model.draft_out_frame(), 50);
    }

    #[test]
    fn from_ranges_keeps_selected_shot_and_draft_marks_separate() {
        let model = TimelineProgressModel::from_ranges(25.0, 200, 80, 10, 150, 40, 90);

        assert_eq!(model.shot_in_frame(), 10);
        assert_eq!(model.shot_out_frame(), 150);
        assert_eq!(model.draft_in_frame(), 40);
        assert_eq!(model.draft_out_frame(), 90);
    }
}
