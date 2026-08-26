//! Sync cover capture component.
//!
//! Optional workflow helper for fast cover slot cutting. It owns session state
//! and frame math only; forms remain UI shells and the broadcast player still
//! receives a regular playlist-input request.

use super::{EditorialProgramPlaybackComponent, EditorialProgramPlaybackInput};
use crate::api::{EditorialPlaylist, EditorialPlaylistCover, EditorialPlaylistSource};
use crate::editorial::segment_program::SegmentProgramModel;
use crate::editorial::types::{MarkerSlot, StoryCover, StoryMarker, StoryShot};
use crate::player_contract::BroadcastSourceTimebase;
use crate::player_remote::BroadcastProgramOpenRequest;
use std::collections::HashMap;

const PREVIEW_COVER_ID: &str = "sync-cover-preview";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SyncCoverCaptureState {
    enabled: bool,
    armed_source: Option<SyncCoverArmedSource>,
    active: Option<SyncCoverSession>,
    pending_slot: Option<SyncCoverPendingSlot>,
    ready_cover: Option<SyncCoverReadyCover>,
}

impl SyncCoverCaptureState {
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn active(&self) -> Option<&SyncCoverSession> {
        self.active.as_ref()
    }

    pub(crate) fn pending_slot(&self) -> Option<&SyncCoverPendingSlot> {
        self.pending_slot.as_ref()
    }

    pub(crate) fn ready_cover(&self) -> Option<&SyncCoverReadyCover> {
        self.ready_cover.as_ref()
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) -> bool {
        let was_active = self.active.is_some();
        self.enabled = enabled;
        if !enabled {
            self.armed_source = None;
            self.active = None;
            self.pending_slot = None;
            self.ready_cover = None;
        }
        was_active
    }

    pub(crate) fn arm_source_in(&mut self, source_clip_id: &str, source_in_frame: i64) {
        let source_clip_id = source_clip_id.trim();
        if !self.enabled || source_clip_id.is_empty() {
            return;
        }
        self.armed_source = Some(SyncCoverArmedSource {
            source_clip_id: source_clip_id.to_string(),
            source_in_frame: source_in_frame.max(0),
        });
        self.active = None;
        self.pending_slot = None;
        self.ready_cover = None;
    }

    pub(crate) fn clear_armed_source(&mut self) {
        self.armed_source = None;
    }

    pub(crate) fn armed_source_in_frame(&self, source_clip_id: &str) -> Option<i64> {
        let source_clip_id = source_clip_id.trim();
        self.armed_source.as_ref().and_then(|armed| {
            (armed.source_clip_id == source_clip_id).then_some(armed.source_in_frame)
        })
    }

    pub(crate) fn set_active(&mut self, session: SyncCoverSession) {
        self.active = Some(session);
        self.armed_source = None;
        self.pending_slot = None;
        self.ready_cover = None;
    }

    pub(crate) fn set_pending_slot(&mut self, pending: SyncCoverPendingSlot) {
        self.pending_slot = Some(pending);
        self.active = None;
        self.armed_source = None;
        self.ready_cover = None;
    }

    pub(crate) fn take_pending_slot(&mut self) -> Option<SyncCoverPendingSlot> {
        self.pending_slot.take()
    }

    pub(crate) fn restore_pending_slot(&mut self, pending: SyncCoverPendingSlot) {
        self.pending_slot = Some(pending);
    }

    pub(crate) fn set_ready_cover(&mut self, ready: SyncCoverReadyCover) {
        self.ready_cover = Some(ready);
        self.pending_slot = None;
        self.active = None;
        self.armed_source = None;
    }

    pub(crate) fn take_ready_cover(&mut self) -> Option<SyncCoverReadyCover> {
        self.ready_cover.take()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncCoverArmedSource {
    pub source_clip_id: String,
    pub source_in_frame: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncCoverSession {
    pub anchor_program_frame: i64,
    pub program_timebase: BroadcastSourceTimebase,
    pub source_clip_id: String,
    pub source_in_frame: i64,
    pub source_duration_frames: i64,
    pub source_timebase: BroadcastSourceTimebase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncCoverPendingSlot {
    pub timeline_start_frame: i64,
    pub timeline_end_frame: i64,
    pub source_clip_id: String,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
    pub source_timebase: BroadcastSourceTimebase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncCoverSlotPlan {
    pub slot_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncCoverReadyCover {
    pub slot_id: String,
    pub timeline_start_frame: i64,
    pub timeline_end_frame: i64,
    pub source_clip_id: String,
    pub source_in_frame: i64,
    pub source_out_frame: i64,
    pub source_timebase: BroadcastSourceTimebase,
}

pub(crate) struct SyncCoverPreviewInput<'a> {
    pub project_id: &'a str,
    pub program_id: &'a str,
    pub start_program_frame: i64,
    pub playlist: Option<&'a EditorialPlaylist>,
    pub marker_slots: &'a [MarkerSlot],
    pub covers: &'a [StoryCover],
    pub markers: &'a [StoryMarker],
    pub all_clips: &'a [StoryShot],
    pub virtual_shots: &'a [StoryShot],
    pub cover_shots: &'a [StoryShot],
    pub playback_inputs: &'a HashMap<String, String>,
    pub source_clip_id: &'a str,
    pub source_in_frame: i64,
    pub source_duration_frames: i64,
    pub source_timebase: BroadcastSourceTimebase,
}

#[derive(Debug, Clone)]
pub(crate) struct SyncCoverPreviewOutcome {
    pub session: SyncCoverSession,
    pub request: BroadcastProgramOpenRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SyncCoverAnchor {
    start_frame: i64,
    limit_frame: i64,
}

pub(crate) struct SyncCoverSpaceContext<'a> {
    pub view_is_source: bool,
    pub source_dock_keyboard_focus: bool,
    pub source_clip_id: &'a str,
}

pub(crate) struct SyncCoverCaptureComponent;

impl SyncCoverCaptureComponent {
    pub(crate) fn auto_arm_source_selection(
        state: &mut SyncCoverCaptureState,
        source_clip_id: &str,
        source_in_frame: i64,
        source_end_frame: i64,
        selected_from_virtual_tab: bool,
    ) {
        state.clear_armed_source();
        if state.enabled() && selected_from_virtual_tab && source_end_frame > source_in_frame.max(0)
        {
            state.arm_source_in(source_clip_id, source_in_frame);
        }
    }

    pub(crate) fn should_start_on_space(
        state: &SyncCoverCaptureState,
        context: SyncCoverSpaceContext<'_>,
    ) -> bool {
        state.enabled()
            && context.view_is_source
            && context.source_dock_keyboard_focus
            && state
                .armed_source_in_frame(context.source_clip_id)
                .is_some()
    }

    pub(crate) fn build_preview(
        input: SyncCoverPreviewInput<'_>,
    ) -> Result<SyncCoverPreviewOutcome, String> {
        let playlist = input
            .playlist
            .ok_or_else(|| "Playlist input nije spreman za Sync".to_string())?;
        if playlist.duration_frames <= 0 {
            return Err("Playlist input je prazan".into());
        }
        let program_timebase = program_timebase_from_playlist(playlist)?;
        if !input.source_timebase.is_valid() {
            return Err("Source timebase još nije potvrđen".into());
        }
        if program_timebase != input.source_timebase {
            return Err("Sync ne miješa program/source timebase".into());
        }
        let source_fps = input
            .source_timebase
            .fps()
            .ok_or_else(|| "Source FPS još nije potvrđen".to_string())?;
        let source_clip_id = input.source_clip_id.trim();
        if source_clip_id.is_empty() {
            return Err("Odaberi source klip za Sync".into());
        }
        let source_duration_frames = input.source_duration_frames.max(1);
        let source_in_frame = input
            .source_in_frame
            .clamp(0, source_duration_frames.saturating_sub(1));
        let available_source_frames = source_duration_frames
            .saturating_sub(source_in_frame)
            .max(1);
        let anchor = marker_anchor(
            input.marker_slots,
            input.markers,
            input.start_program_frame,
            playlist.duration_frames,
        )
        .ok_or_else(|| "Nema M markera za Sync start".to_string())?;
        let anchor_program_frame = anchor.start_frame;
        let preview_end_frame = anchor_program_frame
            .saturating_add(available_source_frames)
            .min(anchor.limit_frame)
            .max(anchor_program_frame + 1);
        let session = SyncCoverSession {
            anchor_program_frame,
            program_timebase,
            source_clip_id: source_clip_id.to_string(),
            source_in_frame,
            source_duration_frames,
            source_timebase: input.source_timebase,
        };
        let preview_source_out =
            source_frame_at_program_frame(&session, preview_end_frame).max(source_in_frame + 1);
        let preview_cover = EditorialPlaylistCover {
            cover_id: PREVIEW_COVER_ID.into(),
            clip_id: source_clip_id.to_string(),
            virtual_shot_id: source_clip_id.to_string(),
            timeline_start_frame: anchor_program_frame,
            timeline_end_frame: preview_end_frame,
            source_in_frame,
            source_out_frame: preview_source_out,
            source_fps,
            source_timebase: crate::api::EditorialSourceTimebase {
                fps_num: i64::from(input.source_timebase.fps_num),
                fps_den: i64::from(input.source_timebase.fps_den),
            },
            streamable: true,
            source: EditorialPlaylistSource {
                kind: "sync_cover_preview".into(),
                cover_id: PREVIEW_COVER_ID.into(),
                virtual_shot_id: source_clip_id.to_string(),
                ..EditorialPlaylistSource::default()
            },
        };
        let preview_playlist = playlist_with_preview_cover(playlist, preview_cover);
        let program = SegmentProgramModel::from_playlist(
            Some(&preview_playlist),
            input.marker_slots,
            input.covers,
            input.markers,
        );
        let clips = input
            .all_clips
            .iter()
            .chain(input.virtual_shots.iter())
            .chain(input.cover_shots.iter())
            .cloned()
            .collect::<Vec<_>>();
        let request =
            EditorialProgramPlaybackComponent::build_program(EditorialProgramPlaybackInput {
                project_id: input.project_id,
                program_id: input.program_id,
                start_program_frame: anchor_program_frame,
                program: &program,
                covers: input.covers,
                clips: &clips,
                playback_inputs: input.playback_inputs,
            })?;

        Ok(SyncCoverPreviewOutcome { session, request })
    }

    pub(crate) fn source_frame_at_program_frame(
        session: &SyncCoverSession,
        program_frame: i64,
    ) -> i64 {
        source_frame_at_program_frame(session, program_frame)
    }

    pub(crate) fn pending_slot(
        session: &SyncCoverSession,
        end_program_frame: i64,
        program_duration_frames: i64,
    ) -> Result<SyncCoverPendingSlot, String> {
        let start = session.anchor_program_frame.max(0);
        let end = end_program_frame
            .max(start + 1)
            .min(program_duration_frames.max(start + 1));
        let source_out = source_frame_at_program_frame(session, end);
        if source_out <= session.source_in_frame {
            return Err("Sync OUT mora biti poslije Source IN".into());
        }
        Ok(SyncCoverPendingSlot {
            timeline_start_frame: start,
            timeline_end_frame: end,
            source_clip_id: session.source_clip_id.clone(),
            source_in_frame: session.source_in_frame,
            source_out_frame: source_out,
            source_timebase: session.source_timebase,
        })
    }

    pub(crate) fn slot_plan(
        pending: &SyncCoverPendingSlot,
        marker_slots: &[MarkerSlot],
    ) -> Result<SyncCoverSlotPlan, String> {
        let Some(slot) = marker_slots.iter().find(|slot| {
            slot.start_frame == pending.timeline_start_frame
                && slot.end_frame == pending.timeline_end_frame
                && !slot.slot_id.trim().is_empty()
        }) else {
            return Err("Sync slot još nije materijaliziran".into());
        };
        Ok(SyncCoverSlotPlan {
            slot_id: slot.slot_id.clone(),
        })
    }

    pub(crate) fn ready_cover(
        pending: &SyncCoverPendingSlot,
        plan: &SyncCoverSlotPlan,
    ) -> SyncCoverReadyCover {
        SyncCoverReadyCover {
            slot_id: plan.slot_id.clone(),
            timeline_start_frame: pending.timeline_start_frame,
            timeline_end_frame: pending.timeline_end_frame,
            source_clip_id: pending.source_clip_id.clone(),
            source_in_frame: pending.source_in_frame,
            source_out_frame: pending.source_out_frame,
            source_timebase: pending.source_timebase,
        }
    }
}

fn marker_anchor(
    marker_slots: &[MarkerSlot],
    markers: &[StoryMarker],
    current_frame: i64,
    duration_frames: i64,
) -> Option<SyncCoverAnchor> {
    let current_frame = current_frame.max(0).min(duration_frames.max(0));
    if let Some(slot) = marker_slots.iter().find(|slot| {
        !slot.slot_id.trim().is_empty()
            && current_frame >= slot.start_frame.max(0)
            && current_frame < slot.end_frame.max(slot.start_frame + 1)
    }) {
        let start = slot.start_frame.max(0);
        return Some(SyncCoverAnchor {
            start_frame: start,
            limit_frame: slot
                .end_frame
                .max(start + 1)
                .min(duration_frames.max(start + 1)),
        });
    }
    markers
        .iter()
        .map(|marker| marker.timeline_frame.max(0))
        .filter(|frame| *frame <= current_frame)
        .max()
        .map(|start| SyncCoverAnchor {
            start_frame: start,
            limit_frame: duration_frames.max(start + 1),
        })
}

fn source_frame_at_program_frame(session: &SyncCoverSession, program_frame: i64) -> i64 {
    debug_assert_eq!(session.program_timebase, session.source_timebase);
    let elapsed_program_frames = program_frame
        .saturating_sub(session.anchor_program_frame)
        .max(0);
    session
        .source_in_frame
        .saturating_add(elapsed_program_frames)
        .clamp(0, session.source_duration_frames.max(0))
}

fn program_timebase_from_playlist(
    playlist: &EditorialPlaylist,
) -> Result<BroadcastSourceTimebase, String> {
    let mut program_timebase = None;
    for segment in &playlist.segments {
        collect_program_timebase(
            segment.source_timebase,
            segment.streamable,
            "Segment",
            &segment.part_id,
            &mut program_timebase,
        )?;
        for cover in &segment.covers {
            collect_program_timebase(
                cover.source_timebase,
                cover.streamable,
                "Pokrivalica",
                &cover.cover_id,
                &mut program_timebase,
            )?;
        }
    }
    program_timebase.ok_or_else(|| "Playlist input nema valjan source timebase".to_string())
}

fn collect_program_timebase(
    source_timebase: crate::api::EditorialSourceTimebase,
    streamable: bool,
    kind: &str,
    id: &str,
    program_timebase: &mut Option<BroadcastSourceTimebase>,
) -> Result<(), String> {
    if !streamable {
        return Ok(());
    }
    let Some(timebase) =
        BroadcastSourceTimebase::from_i64(source_timebase.fps_num, source_timebase.fps_den)
            .filter(|timebase| timebase.is_valid())
    else {
        return Err(format!(
            "{kind} '{}' nema valjan source timebase",
            id.trim()
        ));
    };
    if let Some(existing) = program_timebase {
        if *existing != timebase {
            return Err("Sync ne miješa FPS/timebase u playlist inputu".into());
        }
    } else {
        *program_timebase = Some(timebase);
    }
    Ok(())
}

fn playlist_with_preview_cover(
    playlist: &EditorialPlaylist,
    preview_cover: EditorialPlaylistCover,
) -> EditorialPlaylist {
    let mut playlist = playlist.clone();
    let start = preview_cover.timeline_start_frame.max(0);
    let end = preview_cover.timeline_end_frame.max(start + 1);
    for segment in &mut playlist.segments {
        let segment_start = segment.global_start_frame.max(0);
        let segment_end = segment
            .global_end_frame
            .max(segment_start + segment.duration_frames.max(0))
            .max(segment_start + 1);
        if end > segment_start && start < segment_end {
            segment
                .covers
                .retain(|cover| cover.cover_id != PREVIEW_COVER_ID);
            segment.covers.push(preview_cover.clone());
        }
    }
    playlist
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EditorialPlaylistSegment, EditorialSourceTimebase};
    use crate::player_contract::FrameNumber;
    use crate::player_remote::PROGRAM_AUDIO_OUTPUT_CH2;

    fn tb50() -> BroadcastSourceTimebase {
        BroadcastSourceTimebase {
            fps_num: 50,
            fps_den: 1,
        }
    }

    fn etb50() -> EditorialSourceTimebase {
        EditorialSourceTimebase {
            fps_num: 50,
            fps_den: 1,
        }
    }

    fn playlist() -> EditorialPlaylist {
        EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 50.0,
            duration_frames: 100,
            duration_sec: 2.0,
            segments: vec![EditorialPlaylistSegment {
                part_id: "part_a".into(),
                kind: "tonovi".into(),
                clip_id: "base".into(),
                global_start_frame: 0,
                global_end_frame: 100,
                duration_frames: 100,
                source_in_frame: 100,
                source_out_frame: 200,
                source_fps: 50.0,
                source_timebase: etb50(),
                streamable: true,
                ..EditorialPlaylistSegment::default()
            }],
        }
    }

    fn clip(id: &str, frames: i64) -> StoryShot {
        StoryShot {
            shot_id: id.into(),
            clip_id: id.into(),
            fps: 50.0,
            source_timebase: etb50(),
            duration_frames: frames,
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        }
    }

    #[test]
    fn preview_request_adds_source_as_a2_cover() {
        let mut inputs = HashMap::new();
        inputs.insert("base".into(), "C:/qnc/base.mp4".into());
        inputs.insert("cover".into(), "C:/qnc/cover.mp4".into());

        let outcome = SyncCoverCaptureComponent::build_preview(SyncCoverPreviewInput {
            project_id: "p",
            program_id: "story-sync",
            start_program_frame: 40,
            playlist: Some(&playlist()),
            marker_slots: &[],
            covers: &[],
            markers: &[StoryMarker {
                marker_id: "m0".into(),
                timeline_frame: 0,
                ..StoryMarker::default()
            }],
            all_clips: &[clip("base", 300), clip("cover", 300)],
            virtual_shots: &[],
            cover_shots: &[],
            playback_inputs: &inputs,
            source_clip_id: "cover",
            source_in_frame: 20,
            source_duration_frames: 300,
            source_timebase: tb50(),
        })
        .unwrap();

        assert_eq!(outcome.session.anchor_program_frame, 0);
        assert_eq!(outcome.request.start_program_frame, FrameNumber(0));
        let cover_source = outcome
            .request
            .items
            .iter()
            .flat_map(|item| item.sources.iter())
            .find(|source| {
                source.source_ref.clip_id == "cover"
                    && source.audio_output_channel == Some(PROGRAM_AUDIO_OUTPUT_CH2)
            })
            .expect("sync cover source");
        assert!(cover_source.has_video);
        assert!(cover_source.has_audio);
        assert_eq!(cover_source.source_ref.in_frame, Some(FrameNumber(20)));
    }

    #[test]
    fn marker_supplies_sync_start_when_no_slot_contains_playhead() {
        let mut inputs = HashMap::new();
        inputs.insert("base".into(), "C:/qnc/base.mp4".into());
        inputs.insert("cover".into(), "C:/qnc/cover.mp4".into());

        let outcome = SyncCoverCaptureComponent::build_preview(SyncCoverPreviewInput {
            project_id: "p",
            program_id: "story-sync",
            start_program_frame: 90,
            playlist: Some(&playlist()),
            marker_slots: &[MarkerSlot {
                slot_id: "slot_20_40".into(),
                start_frame: 20,
                end_frame: 40,
                ..MarkerSlot::default()
            }],
            covers: &[],
            markers: &[
                StoryMarker {
                    marker_id: "m0".into(),
                    timeline_frame: 0,
                    ..StoryMarker::default()
                },
                StoryMarker {
                    marker_id: "m90".into(),
                    timeline_frame: 90,
                    ..StoryMarker::default()
                },
            ],
            all_clips: &[clip("base", 300), clip("cover", 300)],
            virtual_shots: &[],
            cover_shots: &[],
            playback_inputs: &inputs,
            source_clip_id: "cover",
            source_in_frame: 20,
            source_duration_frames: 300,
            source_timebase: tb50(),
        })
        .unwrap();

        assert_eq!(outcome.session.anchor_program_frame, 90);
        assert_eq!(outcome.request.start_program_frame, FrameNumber(90));
        let cover_source = outcome
            .request
            .items
            .iter()
            .flat_map(|item| item.sources.iter())
            .find(|source| {
                source.source_ref.clip_id == "cover"
                    && source.audio_output_channel == Some(PROGRAM_AUDIO_OUTPUT_CH2)
            })
            .expect("sync cover source");
        assert_eq!(cover_source.source_ref.in_frame, Some(FrameNumber(20)));
        assert_eq!(cover_source.source_ref.out_frame, Some(FrameNumber(30)));
        let cover_items = outcome
            .request
            .items
            .iter()
            .filter(|item| {
                item.sources.iter().any(|source| {
                    source.source_ref.clip_id == "cover"
                        && source.audio_output_channel == Some(PROGRAM_AUDIO_OUTPUT_CH2)
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(cover_items.len(), 1);
        assert_eq!(cover_items[0].record_in_frame, FrameNumber(90));
        assert_eq!(cover_items[0].record_out_frame, FrameNumber(100));
    }

    #[test]
    fn pending_slot_uses_frame_delta_only() {
        let session = SyncCoverSession {
            anchor_program_frame: 50,
            program_timebase: tb50(),
            source_clip_id: "cover".into(),
            source_in_frame: 20,
            source_duration_frames: 300,
            source_timebase: tb50(),
        };

        let pending = SyncCoverCaptureComponent::pending_slot(&session, 75, 100).unwrap();

        assert_eq!(pending.timeline_start_frame, 50);
        assert_eq!(pending.timeline_end_frame, 75);
        assert_eq!(pending.source_in_frame, 20);
        assert_eq!(pending.source_out_frame, 45);
    }

    #[test]
    fn sync_space_requires_armed_source_context() {
        let mut state = SyncCoverCaptureState::default();
        assert!(!SyncCoverCaptureComponent::should_start_on_space(
            &state,
            SyncCoverSpaceContext {
                view_is_source: true,
                source_dock_keyboard_focus: true,
                source_clip_id: "cover",
            }
        ));

        state.set_enabled(true);
        state.arm_source_in("cover", 10);
        assert!(SyncCoverCaptureComponent::should_start_on_space(
            &state,
            SyncCoverSpaceContext {
                view_is_source: true,
                source_dock_keyboard_focus: true,
                source_clip_id: "cover",
            }
        ));
        assert!(!SyncCoverCaptureComponent::should_start_on_space(
            &state,
            SyncCoverSpaceContext {
                view_is_source: false,
                source_dock_keyboard_focus: true,
                source_clip_id: "cover",
            }
        ));
    }
}
