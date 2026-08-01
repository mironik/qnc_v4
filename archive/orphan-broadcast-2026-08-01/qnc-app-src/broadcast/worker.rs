//! Decode worker adapter.
//!
//! This layer intentionally has no clock. It consumes `FrameDecodeBatch` and
//! fills queues. Playback time remains owned by the carrier/program clock.

use super::asset::{
    BroadcastMediaResolver, BroadcastResolvedDecodeBackend, ResolvedFrameDecodePlan,
};
use super::audio::AudioFrameQueue;
use super::backend::{BroadcastDecodeBackend, DecodeError, DecodedAudioBus, DecodedProgramFrame};
use super::video::VideoFrameQueue;
use super::window::FrameDecodeBatch;

#[derive(Debug)]
pub struct BroadcastDecodeWorker<B> {
    backend: B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeQueueFill {
    pub video_frames: usize,
    pub audio_frames: usize,
}

impl<B> BroadcastDecodeWorker<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }
}

impl<B: BroadcastDecodeBackend> BroadcastDecodeWorker<B> {
    pub fn decode_batch(
        &mut self,
        batch: &FrameDecodeBatch,
    ) -> Result<Vec<DecodedProgramFrame<B::VideoPayload, B::AudioPayload>>, DecodeError> {
        batch.decode_with(&mut self.backend)
    }

    pub fn fill_video_queue(
        &mut self,
        batch: &FrameDecodeBatch,
        queue: &mut VideoFrameQueue<DecodedProgramFrame<B::VideoPayload, B::AudioPayload>>,
    ) -> Result<usize, DecodeError> {
        let decoded = self.decode_batch(batch)?;
        let count = decoded.len();
        for frame in decoded {
            queue.push_decoded(frame.source_frame, frame);
        }
        Ok(count)
    }

    pub fn fill_audio_queue(
        &mut self,
        batch: &FrameDecodeBatch,
        queue: &mut AudioFrameQueue<Vec<DecodedAudioBus<B::AudioPayload>>>,
    ) -> Result<usize, DecodeError> {
        let decoded = self.decode_batch(batch)?;
        let mut count = 0;
        for frame in decoded {
            if frame.audio.is_empty() {
                continue;
            }
            queue.push_decoded(frame.source_frame, frame.audio);
            count += 1;
        }
        Ok(count)
    }
}

#[derive(Debug)]
pub struct BroadcastResolvedDecodeWorker<B, R> {
    backend: B,
    resolver: R,
}

impl<B, R> BroadcastResolvedDecodeWorker<B, R> {
    pub fn new(backend: B, resolver: R) -> Self {
        Self { backend, resolver }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn resolver(&self) -> &R {
        &self.resolver
    }
}

impl<B, R> BroadcastResolvedDecodeWorker<B, R>
where
    B: BroadcastResolvedDecodeBackend,
    R: BroadcastMediaResolver,
{
    pub fn decode_batch(
        &mut self,
        batch: &FrameDecodeBatch,
    ) -> Result<Vec<DecodedProgramFrame<B::VideoPayload, B::AudioPayload>>, DecodeError> {
        let mut out = Vec::with_capacity(batch.plans.len());
        for plan in &batch.plans {
            let resolved = ResolvedFrameDecodePlan::resolve(plan, &self.resolver)
                .map_err(|err| DecodeError::new(err.message))?;
            let decoded = self.backend.decode_resolved_frame(&resolved)?;
            decoded.validate_against_plan(plan)?;
            out.push(decoded);
        }
        Ok(out)
    }

    pub fn fill_video_queue(
        &mut self,
        batch: &FrameDecodeBatch,
        queue: &mut VideoFrameQueue<DecodedProgramFrame<B::VideoPayload, B::AudioPayload>>,
    ) -> Result<usize, DecodeError> {
        let decoded = self.decode_batch(batch)?;
        let count = decoded.len();
        for frame in decoded {
            queue.push_decoded(frame.source_frame, frame);
        }
        Ok(count)
    }

    pub fn fill_audio_queue(
        &mut self,
        batch: &FrameDecodeBatch,
        queue: &mut AudioFrameQueue<Vec<DecodedAudioBus<B::AudioPayload>>>,
    ) -> Result<usize, DecodeError> {
        let decoded = self.decode_batch(batch)?;
        let mut count = 0;
        for frame in decoded {
            if frame.audio.is_empty() {
                continue;
            }
            queue.push_decoded(frame.source_frame, frame.audio);
            count += 1;
        }
        Ok(count)
    }

    pub fn fill_queues(
        &mut self,
        batch: &FrameDecodeBatch,
        video_queue: &mut VideoFrameQueue<DecodedProgramFrame<B::VideoPayload, B::AudioPayload>>,
        audio_queue: &mut AudioFrameQueue<Vec<DecodedAudioBus<B::AudioPayload>>>,
    ) -> Result<DecodeQueueFill, DecodeError>
    where
        B::VideoPayload: Clone,
        B::AudioPayload: Clone,
    {
        let decoded = self.decode_batch(batch)?;
        let mut fill = DecodeQueueFill {
            video_frames: 0,
            audio_frames: 0,
        };
        for frame in decoded {
            if !frame.audio.is_empty() {
                audio_queue.push_decoded(frame.source_frame, frame.audio.clone());
                fill.audio_frames += 1;
            }
            video_queue.push_decoded(frame.source_frame, frame);
            fill.video_frames += 1;
        }
        Ok(fill)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::timebase::{FrameNumber, FrameRange, Timebase};
    use crate::broadcast::AudioFrameQueue;
    use crate::broadcast::{
        AudioChannel, AudioLayerSourceSpec, BroadcastFrameScheduler, BroadcastMediaAsset,
        BroadcastProgramGraph, BroadcastRenderPlan, CelluloidTrack, FilmstripUnderlay,
        InMemoryMediaResolver, NullBroadcastBackend, NullResolvedBroadcastBackend,
        UniversalTimelineSpec, VideoFrameQueue, VideoLayerSourceSpec, VirtualMediaRef,
    };

    fn batch() -> FrameDecodeBatch {
        let carrier = CelluloidTrack::new(
            "project",
            "timeline",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(10), FrameNumber(20)),
        );
        let mut spec =
            UniversalTimelineSpec::new(carrier).with_filmstrip(FilmstripUnderlay::Hidden);
        spec = spec.with_base_video(VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new(
            "base",
            "clip_base",
        )));
        spec.add_audio_track(AudioChannel::new(1).unwrap(), AudioLayerSourceSpec::Silence);
        let plan =
            BroadcastRenderPlan::from_graph(&BroadcastProgramGraph::from_universal_timeline(spec));
        let scheduler = BroadcastFrameScheduler::new(plan.clone());
        FrameDecodeBatch::from_scheduler(&plan, &scheduler, FrameNumber(10), 3)
    }

    #[test]
    fn worker_decodes_batch_without_owning_clock() {
        let mut worker = BroadcastDecodeWorker::new(NullBroadcastBackend);
        let decoded = worker.decode_batch(&batch()).unwrap();

        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].source_frame, FrameNumber(10));
        assert_eq!(decoded[1].source_frame, FrameNumber(11));
        assert_eq!(decoded[2].source_frame, FrameNumber(12));
    }

    #[test]
    fn worker_fills_video_queue_from_decode_batch() {
        let mut worker = BroadcastDecodeWorker::new(NullBroadcastBackend);
        let mut queue = VideoFrameQueue::new(8);

        let count = worker.fill_video_queue(&batch(), &mut queue).unwrap();

        assert_eq!(count, 3);
        assert_eq!(queue.len(), 3);
        let frame = queue.frame_for_program_clock(FrameNumber(11)).unwrap();
        assert_eq!(frame.frame, FrameNumber(11));
        assert_eq!(frame.payload.source_frame, FrameNumber(11));
    }

    #[test]
    fn worker_fills_audio_queue_from_decode_batch_without_owning_clock() {
        let mut worker = BroadcastDecodeWorker::new(NullBroadcastBackend);
        let mut queue = AudioFrameQueue::new(8);

        let count = worker.fill_audio_queue(&batch(), &mut queue).unwrap();

        assert_eq!(count, 3);
        assert_eq!(queue.len(), 3);
        let frame = queue.frame_for_program_clock(FrameNumber(11)).unwrap();
        assert_eq!(frame.frame, FrameNumber(11));
        assert_eq!(frame.payload.len(), 1);
        assert_eq!(frame.payload[0].source_frame, FrameNumber(11));
    }

    fn media_resolver() -> InMemoryMediaResolver {
        InMemoryMediaResolver::new().with_asset(BroadcastMediaAsset::proxy_local(
            "project",
            "base",
            "clip_base",
            "media/base_proxy.mxf",
            Timebase::from_source_fps(25.0),
            true,
            true,
        ))
    }

    #[test]
    fn resolved_worker_resolves_batch_before_decoding() {
        let mut worker =
            BroadcastResolvedDecodeWorker::new(NullResolvedBroadcastBackend, media_resolver());

        let decoded = worker.decode_batch(&batch()).unwrap();

        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].source_frame, FrameNumber(10));
        assert_eq!(decoded[0].video.len(), 1);
        assert_eq!(decoded[0].audio.len(), 1);
    }

    #[test]
    fn resolved_worker_reports_missing_asset_as_decode_error() {
        let mut worker = BroadcastResolvedDecodeWorker::new(
            NullResolvedBroadcastBackend,
            InMemoryMediaResolver::new(),
        );

        let err = worker.decode_batch(&batch()).unwrap_err();

        assert!(err.message.contains("missing media asset"));
    }

    #[test]
    fn resolved_worker_can_fill_video_and_audio_queues_in_one_decode_pass() {
        let mut worker =
            BroadcastResolvedDecodeWorker::new(NullResolvedBroadcastBackend, media_resolver());
        let mut video_queue = VideoFrameQueue::new(8);
        let mut audio_queue = AudioFrameQueue::new(8);

        let fill = worker
            .fill_queues(&batch(), &mut video_queue, &mut audio_queue)
            .unwrap();

        assert_eq!(
            fill,
            DecodeQueueFill {
                video_frames: 3,
                audio_frames: 3
            }
        );
        assert_eq!(video_queue.len(), 3);
        assert_eq!(audio_queue.len(), 3);
    }
}
