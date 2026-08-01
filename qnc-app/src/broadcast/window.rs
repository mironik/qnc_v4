//! Decode lookahead window.
//!
//! Decoder workers do not own playback time. They receive a bounded frame
//! window derived from the carrier/program clock and fill queues ahead of the
//! current frame.

use super::backend::{BroadcastDecodeBackend, DecodeError, DecodedProgramFrame, FrameDecodePlan};
use super::render::BroadcastRenderPlan;
use super::schedule::BroadcastFrameScheduler;
use super::timebase::FrameNumber;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeWindow {
    pub start: FrameNumber,
    pub end_exclusive: FrameNumber,
}

impl DecodeWindow {
    pub fn len(self) -> usize {
        (self.end_exclusive.0 - self.start.0).max(0) as usize
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameDecodeBatch {
    pub window: DecodeWindow,
    pub plans: Vec<FrameDecodePlan>,
}

impl FrameDecodeBatch {
    pub fn from_scheduler(
        render_plan: &BroadcastRenderPlan,
        scheduler: &BroadcastFrameScheduler,
        start_frame: FrameNumber,
        lookahead_frames: usize,
    ) -> Self {
        let carrier = &render_plan.carrier;
        let start = carrier.clamp_source_frame(start_frame);
        let count = lookahead_frames.max(1) as i64;
        let end = FrameNumber((start.0 + count).min(carrier.source_range.end_exclusive.0));
        let end = FrameNumber(end.0.max(start.0 + 1));

        let mut plans = Vec::new();
        for frame in start.0..end.0 {
            let scheduled = scheduler.schedule_frame(FrameNumber(frame));
            plans.push(FrameDecodePlan::from_scheduled(render_plan, scheduled));
        }

        Self {
            window: DecodeWindow {
                start,
                end_exclusive: end,
            },
            plans,
        }
    }

    pub fn decode_with<B: BroadcastDecodeBackend>(
        &self,
        backend: &mut B,
    ) -> Result<Vec<DecodedProgramFrame<B::VideoPayload, B::AudioPayload>>, DecodeError> {
        let mut out = Vec::with_capacity(self.plans.len());
        for plan in &self.plans {
            let decoded = backend.decode_frame(plan)?;
            decoded.validate_against_plan(plan)?;
            out.push(decoded);
        }
        Ok(out)
    }

    pub fn has_filmstrip_decode_input(&self) -> bool {
        self.plans
            .iter()
            .any(FrameDecodePlan::has_filmstrip_decode_input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        AudioChannel, AudioLayerSourceSpec, BroadcastFrameScheduler, BroadcastProgramGraph,
        BroadcastRenderPlan, CelluloidTrack, FilmstripUnderlay, NullBroadcastBackend,
        UniversalTimelineSpec, VideoLayerSourceSpec, VirtualMediaRef,
    };

    fn render_plan() -> BroadcastRenderPlan {
        let carrier = CelluloidTrack::new(
            "project",
            "timeline",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(10), FrameNumber(15)),
        );
        let mut spec =
            UniversalTimelineSpec::new(carrier).with_filmstrip(FilmstripUnderlay::Hidden);
        spec = spec.with_base_video(VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new(
            "base",
            "clip_base",
        )));
        spec.add_audio_track(AudioChannel::new(1).unwrap(), AudioLayerSourceSpec::Silence);
        BroadcastRenderPlan::from_graph(&BroadcastProgramGraph::from_universal_timeline(spec))
    }

    #[test]
    fn decode_window_clamps_to_carrier_range() {
        let plan = render_plan();
        let scheduler = BroadcastFrameScheduler::new(plan.clone());
        let batch = FrameDecodeBatch::from_scheduler(&plan, &scheduler, FrameNumber(13), 10);

        assert_eq!(batch.window.start, FrameNumber(13));
        assert_eq!(batch.window.end_exclusive, FrameNumber(15));
        assert_eq!(batch.plans.len(), 2);
        assert_eq!(batch.plans[0].source_frame, FrameNumber(13));
        assert_eq!(batch.plans[1].source_frame, FrameNumber(14));
    }

    #[test]
    fn decode_window_has_no_filmstrip_inputs() {
        let plan = render_plan();
        let scheduler = BroadcastFrameScheduler::new(plan.clone());
        let batch = FrameDecodeBatch::from_scheduler(&plan, &scheduler, FrameNumber(10), 3);

        assert!(!batch.has_filmstrip_decode_input());
    }

    #[test]
    fn decode_window_decodes_with_null_backend() {
        let plan = render_plan();
        let scheduler = BroadcastFrameScheduler::new(plan.clone());
        let batch = FrameDecodeBatch::from_scheduler(&plan, &scheduler, FrameNumber(10), 3);
        let mut backend = NullBroadcastBackend;

        let decoded = batch.decode_with(&mut backend).unwrap();
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].source_frame, FrameNumber(10));
        assert_eq!(decoded[1].source_frame, FrameNumber(11));
        assert_eq!(decoded[2].source_frame, FrameNumber(12));
    }

    struct WrongPtsBackend;

    impl BroadcastDecodeBackend for WrongPtsBackend {
        type VideoPayload = ();
        type AudioPayload = ();

        fn decode_frame(
            &mut self,
            plan: &FrameDecodePlan,
        ) -> Result<DecodedProgramFrame<Self::VideoPayload, Self::AudioPayload>, DecodeError>
        {
            let mut decoded = NullBroadcastBackend.decode_frame(plan)?;
            decoded.pts_sec += 0.04;
            Ok(decoded)
        }
    }

    #[test]
    fn decode_window_rejects_backend_output_on_wrong_pts() {
        let plan = render_plan();
        let scheduler = BroadcastFrameScheduler::new(plan.clone());
        let batch = FrameDecodeBatch::from_scheduler(&plan, &scheduler, FrameNumber(10), 1);
        let mut backend = WrongPtsBackend;

        let err = batch.decode_with(&mut backend).unwrap_err();
        assert!(err.message.contains("does not match requested PTS"));
    }
}
