//! Broadcast playback runtime tick.
//!
//! The runtime driver is the coordination layer between the master clock,
//! presentation decisions, decoder lookahead and final playout diagnostics.
//! It does not decode frames, does not own time and does not treat audio or
//! video as the clock. A real media backend can consume the returned
//! `FrameDecodeBatch` and fill queues separately.

use std::time::Instant;

use super::diagnostics::BroadcastPlayoutDiagnostics;
use super::playout::BroadcastPlayoutFrame;
use super::presentation::{BroadcastPresentationBatch, PresentationPlanError};
use super::session::BroadcastPlaybackSession;
use super::timebase::FrameNumber;
use super::window::FrameDecodeBatch;

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastRuntimeTick<T> {
    pub master_frame: FrameNumber,
    pub decode_batch: FrameDecodeBatch,
    pub presentation_batch: BroadcastPresentationBatch,
    pub playout: Option<BroadcastPlayoutFrame<T>>,
    pub diagnostics: BroadcastPlayoutDiagnostics,
}

pub struct BroadcastRuntimeDriver;

impl BroadcastRuntimeDriver {
    pub fn tick<V: Clone, A>(
        session: &mut BroadcastPlaybackSession<V, A>,
        now: Instant,
        lookahead_frames: usize,
    ) -> Result<BroadcastRuntimeTick<V>, PresentationPlanError> {
        let master_frame = session.current_source_frame(now);
        let decode_batch = session.decode_batch_current_frame(now, lookahead_frames);
        let presentation_batch = BroadcastPresentationBatch::try_from_decode_batch(&decode_batch)?;

        session.push_presentation_batch(presentation_batch.clone());
        let playout = session.playout_frame(now);
        let diagnostics = session.playout_diagnostics(master_frame, playout.as_ref());

        Ok(BroadcastRuntimeTick {
            master_frame,
            decode_batch,
            presentation_batch,
            playout,
            diagnostics,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::broadcast::clock::ClockReference;
    use crate::broadcast::diagnostics::PlayoutProblem;
    use crate::broadcast::playout::PlayoutReadiness;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::BroadcastPlaybackSource;

    fn source(has_audio: bool, has_video: bool) -> BroadcastPlaybackSource {
        BroadcastPlaybackSource {
            project_id: "project".into(),
            virtual_shot_id: "shot".into(),
            clip_id: "clip".into(),
            source_range: FrameRange::new(FrameNumber(100), FrameNumber(200)),
            source_timebase: Timebase::from_source_fps(25.0),
            has_video,
            has_audio,
            audio_channels: if has_audio { 1 } else { 0 },
        }
    }

    #[test]
    fn runtime_tick_prepares_decode_and_presentation_lookahead_from_master_clock() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 8, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);

        let tick =
            BroadcastRuntimeDriver::tick(&mut session, t0 + Duration::from_millis(80), 4).unwrap();

        assert_eq!(tick.master_frame, FrameNumber(102));
        assert_eq!(tick.decode_batch.window.start, FrameNumber(102));
        assert_eq!(tick.decode_batch.plans.len(), 4);
        assert_eq!(tick.presentation_batch.window.start, FrameNumber(102));
        assert_eq!(tick.presentation_batch.len(), 4);
        assert_eq!(session.queued_presentation_len(), 4);
        assert_eq!(tick.diagnostics.problem, Some(PlayoutProblem::VideoMissing));
    }

    #[test]
    fn runtime_tick_allows_audio_only_program_without_video_payload() {
        let mut session: BroadcastPlaybackSession<&'static str> = BroadcastPlaybackSession::new(
            source(true, false),
            8,
            ClockReference::InternalMonotonic,
        );
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);

        let tick = BroadcastRuntimeDriver::tick(&mut session, t0, 3).unwrap();

        assert_eq!(tick.master_frame, FrameNumber(100));
        assert!(tick.decode_batch.plans[0].video.is_empty());
        assert_eq!(
            tick.playout.unwrap().readiness(),
            PlayoutReadiness::AudioOnly
        );
        assert_eq!(tick.diagnostics.problem, None);
        assert!(!tick.diagnostics.video_required);
    }

    #[test]
    fn runtime_tick_uses_existing_decoded_video_payload_without_changing_clock() {
        let mut session: BroadcastPlaybackSession<&'static str> =
            BroadcastPlaybackSession::new(source(true, true), 8, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        session.play_from_source_frame(FrameNumber(100), t0);
        session.push_decoded_video_frame(FrameNumber(100), "f100");

        let tick = BroadcastRuntimeDriver::tick(&mut session, t0, 3).unwrap();

        assert_eq!(tick.master_frame, FrameNumber(100));
        assert_eq!(tick.playout.unwrap().readiness(), PlayoutReadiness::Clean);
        assert_eq!(tick.diagnostics.problem, None);
    }
}
