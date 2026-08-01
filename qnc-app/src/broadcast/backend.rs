//! Backend-neutral broadcast decode contract.
//!
//! Media backends consume `FrameDecodePlan`. The plan is already filtered by
//! carrier frame and contains only real decoder inputs: base/overlay video and
//! audio buses. UI underlays such as filmstrip are intentionally absent.

use super::layers::{AudioChannel, AudioMix, EffectKind, MarkerKind, ZPriority};
use super::render::{
    AudioRenderSource, BroadcastRenderPlan, EffectRenderEvent, MarkerRenderEvent, VideoRenderRole,
    VideoRenderSource,
};
use super::schedule::ScheduledProgramFrame;
use super::sync::{AudioSampleSpan, BROADCAST_AUDIO_SAMPLE_RATE_HZ};
use super::timebase::FrameNumber;

#[derive(Debug, Clone, PartialEq)]
pub struct VideoDecodeRequest {
    pub layer_id: String,
    pub role: VideoRenderRole,
    pub z_priority: ZPriority,
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub media_seek_sec: f64,
    pub source: VideoRenderSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioDecodeRequest {
    pub layer_id: String,
    pub channel: AudioChannel,
    pub mix: AudioMix,
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub media_seek_sec: f64,
    pub sample_rate_hz: u32,
    pub sample_span: AudioSampleSpan,
    pub source: AudioRenderSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodeMarkerEvent {
    pub marker_id: String,
    pub kind: MarkerKind,
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodeEffectEvent {
    pub marker_id: String,
    pub effect: EffectKind,
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameDecodePlan {
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub video: Vec<VideoDecodeRequest>,
    pub audio: Vec<AudioDecodeRequest>,
    pub markers: Vec<DecodeMarkerEvent>,
    pub effects: Vec<DecodeEffectEvent>,
}

impl FrameDecodePlan {
    pub fn from_scheduled(
        render_plan: &BroadcastRenderPlan,
        scheduled: ScheduledProgramFrame,
    ) -> Self {
        let source_frame = render_plan
            .carrier
            .clamp_source_frame(scheduled.source_frame);
        let pts_sec = render_plan
            .carrier
            .program_seconds_at_source_frame(source_frame);
        let media_seek_sec = render_plan.carrier.timebase.seconds_at_frame(source_frame);
        let audio_sample_span = AudioSampleSpan::from_carrier_frame(
            &render_plan.carrier,
            source_frame,
            BROADCAST_AUDIO_SAMPLE_RATE_HZ,
        );

        Self {
            source_frame,
            pts_sec,
            video: scheduled
                .video_layers
                .into_iter()
                .map(|layer| VideoDecodeRequest {
                    layer_id: layer.layer_id,
                    role: layer.role,
                    z_priority: layer.z_priority,
                    source_frame,
                    pts_sec,
                    media_seek_sec,
                    source: layer.source,
                })
                .collect(),
            audio: scheduled
                .audio_buses
                .into_iter()
                .map(|bus| AudioDecodeRequest {
                    layer_id: bus.layer_id,
                    channel: bus.channel,
                    mix: bus.mix,
                    source_frame,
                    pts_sec,
                    media_seek_sec,
                    sample_rate_hz: BROADCAST_AUDIO_SAMPLE_RATE_HZ,
                    sample_span: audio_sample_span,
                    source: bus.source,
                })
                .collect(),
            markers: scheduled
                .markers
                .into_iter()
                .map(|event| marker_event(event, pts_sec))
                .collect(),
            effects: scheduled
                .effects
                .into_iter()
                .map(|event| effect_event(event, pts_sec))
                .collect(),
        }
    }

    pub fn has_filmstrip_decode_input(&self) -> bool {
        self.video
            .iter()
            .any(|request| request.layer_id.contains("filmstrip"))
    }
}

fn marker_event(event: MarkerRenderEvent, pts_sec: f64) -> DecodeMarkerEvent {
    DecodeMarkerEvent {
        marker_id: event.marker_id,
        kind: event.kind,
        source_frame: event.frame,
        pts_sec,
    }
}

fn effect_event(event: EffectRenderEvent, pts_sec: f64) -> DecodeEffectEvent {
    DecodeEffectEvent {
        marker_id: event.marker_id,
        effect: event.effect,
        source_frame: event.frame,
        pts_sec,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    pub message: String,
}

impl DecodeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedVideoLayer<T> {
    pub layer_id: String,
    pub role: VideoRenderRole,
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub media_seek_sec: f64,
    pub payload: Option<T>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudioBus<T> {
    pub layer_id: String,
    pub channel: AudioChannel,
    pub mix: AudioMix,
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub media_seek_sec: f64,
    pub sample_rate_hz: u32,
    pub sample_span: AudioSampleSpan,
    pub payload: Option<T>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedProgramFrame<V, A> {
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub video: Vec<DecodedVideoLayer<V>>,
    pub audio: Vec<DecodedAudioBus<A>>,
    pub markers: Vec<DecodeMarkerEvent>,
    pub effects: Vec<DecodeEffectEvent>,
}

impl<V, A> DecodedProgramFrame<V, A> {
    pub fn validate_against_plan(&self, plan: &FrameDecodePlan) -> Result<(), DecodeError> {
        if self.source_frame != plan.source_frame {
            return Err(DecodeError::new(format!(
                "decoded frame {:?} does not match requested frame {:?}",
                self.source_frame, plan.source_frame
            )));
        }
        if !same_pts(self.pts_sec, plan.pts_sec) {
            return Err(DecodeError::new(format!(
                "decoded frame PTS {} does not match requested PTS {}",
                self.pts_sec, plan.pts_sec
            )));
        }

        for decoded in &self.video {
            if decoded.source_frame != plan.source_frame {
                return Err(DecodeError::new(format!(
                    "decoded video layer '{}' is on frame {:?}, expected {:?}",
                    decoded.layer_id, decoded.source_frame, plan.source_frame
                )));
            }
            if !same_pts(decoded.pts_sec, plan.pts_sec) {
                return Err(DecodeError::new(format!(
                    "decoded video layer '{}' is on PTS {}, expected {}",
                    decoded.layer_id, decoded.pts_sec, plan.pts_sec
                )));
            }
            let Some(request) = plan
                .video
                .iter()
                .find(|request| request.layer_id == decoded.layer_id)
            else {
                return Err(DecodeError::new(format!(
                    "decoded video layer '{}' was not requested",
                    decoded.layer_id
                )));
            };
            if !same_pts(decoded.media_seek_sec, request.media_seek_sec) {
                return Err(DecodeError::new(format!(
                    "decoded video layer '{}' media seek {} does not match requested {}",
                    decoded.layer_id, decoded.media_seek_sec, request.media_seek_sec
                )));
            }
        }

        for decoded in &self.audio {
            if decoded.source_frame != plan.source_frame {
                return Err(DecodeError::new(format!(
                    "decoded audio bus '{}' is on frame {:?}, expected {:?}",
                    decoded.layer_id, decoded.source_frame, plan.source_frame
                )));
            }
            if !same_pts(decoded.pts_sec, plan.pts_sec) {
                return Err(DecodeError::new(format!(
                    "decoded audio bus '{}' is on PTS {}, expected {}",
                    decoded.layer_id, decoded.pts_sec, plan.pts_sec
                )));
            }
            if !plan.audio.iter().any(|request| {
                request.layer_id == decoded.layer_id && request.channel == decoded.channel
            }) {
                return Err(DecodeError::new(format!(
                    "decoded audio bus '{}' on channel {:?} was not requested",
                    decoded.layer_id, decoded.channel
                )));
            }
            let Some(request) = plan.audio.iter().find(|request| {
                request.layer_id == decoded.layer_id && request.channel == decoded.channel
            }) else {
                continue;
            };
            if !same_pts(decoded.media_seek_sec, request.media_seek_sec) {
                return Err(DecodeError::new(format!(
                    "decoded audio bus '{}' media seek {} does not match requested {}",
                    decoded.layer_id, decoded.media_seek_sec, request.media_seek_sec
                )));
            }
            if decoded.sample_rate_hz != request.sample_rate_hz {
                return Err(DecodeError::new(format!(
                    "decoded audio bus '{}' sample rate {} does not match requested {}",
                    decoded.layer_id, decoded.sample_rate_hz, request.sample_rate_hz
                )));
            }
            if decoded.sample_span != request.sample_span {
                return Err(DecodeError::new(format!(
                    "decoded audio bus '{}' sample span {:?} does not match requested {:?}",
                    decoded.layer_id, decoded.sample_span, request.sample_span
                )));
            }
        }

        Ok(())
    }
}

fn same_pts(left: f64, right: f64) -> bool {
    (left - right).abs() < 0.000_000_001
}

pub trait BroadcastDecodeBackend {
    type VideoPayload;
    type AudioPayload;

    fn decode_frame(
        &mut self,
        plan: &FrameDecodePlan,
    ) -> Result<DecodedProgramFrame<Self::VideoPayload, Self::AudioPayload>, DecodeError>;
}

#[derive(Debug, Default)]
pub struct NullBroadcastBackend;

impl BroadcastDecodeBackend for NullBroadcastBackend {
    type VideoPayload = ();
    type AudioPayload = ();

    fn decode_frame(
        &mut self,
        plan: &FrameDecodePlan,
    ) -> Result<DecodedProgramFrame<Self::VideoPayload, Self::AudioPayload>, DecodeError> {
        if plan.has_filmstrip_decode_input() {
            return Err(DecodeError::new("filmstrip is not a decoder input"));
        }

        Ok(DecodedProgramFrame {
            source_frame: plan.source_frame,
            pts_sec: plan.pts_sec,
            video: plan
                .video
                .iter()
                .map(|request| DecodedVideoLayer {
                    layer_id: request.layer_id.clone(),
                    role: request.role,
                    source_frame: request.source_frame,
                    pts_sec: request.pts_sec,
                    media_seek_sec: request.media_seek_sec,
                    payload: None,
                })
                .collect(),
            audio: plan
                .audio
                .iter()
                .map(|request| DecodedAudioBus {
                    layer_id: request.layer_id.clone(),
                    channel: request.channel,
                    mix: request.mix,
                    source_frame: request.source_frame,
                    pts_sec: request.pts_sec,
                    media_seek_sec: request.media_seek_sec,
                    sample_rate_hz: request.sample_rate_hz,
                    sample_span: request.sample_span,
                    payload: None,
                })
                .collect(),
            markers: plan.markers.clone(),
            effects: plan.effects.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        AudioChannel, AudioLayerSourceSpec, BroadcastFrameScheduler, BroadcastPlaybackSource,
        BroadcastProgramGraph, BroadcastRenderPlan, CelluloidTrack, FilmstripUnderlay, MarkerKind,
        UniversalTimelineSpec, VideoLayerSourceSpec, VirtualMediaRef,
    };

    fn source() -> BroadcastPlaybackSource {
        BroadcastPlaybackSource {
            project_id: "project".into(),
            virtual_shot_id: "shot".into(),
            clip_id: "clip".into(),
            source_range: FrameRange::new(FrameNumber(100), FrameNumber(200)),
            source_timebase: Timebase::from_source_fps(25.0),
            has_video: true,
            has_audio: true,
            audio_channels: 2,
        }
    }

    #[test]
    fn decode_plan_uses_carrier_pts_and_excludes_filmstrip() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source());
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let scheduled = scheduler.schedule_frame(FrameNumber(125));
        let decode_plan = FrameDecodePlan::from_scheduled(&render_plan, scheduled);
        let expected_media_seek = render_plan
            .carrier
            .timebase
            .seconds_at_frame(FrameNumber(125));

        assert_eq!(decode_plan.source_frame, FrameNumber(125));
        assert_eq!(decode_plan.pts_sec, 1.0);
        assert_eq!(decode_plan.video[0].media_seek_sec, expected_media_seek);
        assert_eq!(decode_plan.video.len(), 1);
        assert!(!decode_plan.has_filmstrip_decode_input());
        assert!(matches!(
            decode_plan.video[0].source,
            VideoRenderSource::VirtualShot { .. }
        ));
    }

    #[test]
    fn decode_plan_carries_audio_buses_and_overlay_layers() {
        let carrier = CelluloidTrack::new(
            "project",
            "timeline",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec =
            UniversalTimelineSpec::new(carrier).with_filmstrip(FilmstripUnderlay::Hidden);
        spec = spec.with_base_video(VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new(
            "base",
            "clip_base",
        )));
        spec.add_overlay_range(
            1,
            VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new("cover", "clip_cover")),
            FrameRange::new(FrameNumber(25), FrameNumber(50)),
        );
        spec.add_audio_track(
            AudioChannel::new(1).unwrap(),
            AudioLayerSourceSpec::VirtualShot(VirtualMediaRef::new("audio", "clip_audio")),
        );
        spec.add_marker("m1", MarkerKind::M, FrameNumber(30));

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(30)),
        );

        assert_eq!(decode_plan.video.len(), 2);
        assert_eq!(decode_plan.audio.len(), 1);
        assert_eq!(decode_plan.markers.len(), 1);
    }

    #[test]
    fn audio_bus_uses_same_carrier_pts_without_visual_z_priority() {
        let carrier = CelluloidTrack::new(
            "project",
            "timeline",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(100), FrameNumber(200)),
        );
        let mut spec =
            UniversalTimelineSpec::new(carrier).with_filmstrip(FilmstripUnderlay::Hidden);
        spec = spec.with_base_video(VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new(
            "base",
            "clip_base",
        )));
        spec.add_overlay_range(
            1,
            VideoLayerSourceSpec::VirtualShot(VirtualMediaRef::new("cover", "clip_cover")),
            FrameRange::new(FrameNumber(120), FrameNumber(150)),
        );
        spec.add_audio_track(
            AudioChannel::new(2).unwrap(),
            AudioLayerSourceSpec::VirtualShot(VirtualMediaRef::new("audio", "clip_audio")),
        );

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(125)),
        );
        let expected_media_seek = render_plan
            .carrier
            .timebase
            .seconds_at_frame(FrameNumber(125));

        assert_eq!(decode_plan.source_frame, FrameNumber(125));
        assert_eq!(decode_plan.pts_sec, 1.0);
        assert_eq!(decode_plan.video.len(), 2);
        assert_eq!(decode_plan.video[0].z_priority, ZPriority::BASE_VIDEO);
        assert_eq!(decode_plan.video[1].z_priority, ZPriority::new(1_001));
        assert_eq!(decode_plan.audio.len(), 1);
        assert_eq!(decode_plan.audio[0].channel.get(), 2);
        assert_eq!(decode_plan.audio[0].source_frame, decode_plan.source_frame);
        assert_eq!(decode_plan.audio[0].pts_sec, decode_plan.pts_sec);
        assert_eq!(decode_plan.audio[0].media_seek_sec, expected_media_seek);
        assert_eq!(decode_plan.audio[0].sample_rate_hz, 48_000);
        assert_eq!(
            decode_plan.audio[0].sample_span,
            AudioSampleSpan::new(48_000, 49_920)
        );
    }

    #[test]
    fn decode_plan_carries_independent_a1_and_per_cover_a2_mix() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        let cover_range = FrameRange::new(FrameNumber(25), FrameNumber(50));
        spec.add_off_vo_audio_with_mix(
            VirtualMediaRef::new("vo", "clip_vo"),
            AudioMix::with_gain_db_tenths(-60),
        );
        spec.add_cover_overlay_with_audio_mix(
            1,
            VirtualMediaRef::new("cover_video", "clip_cover_video"),
            Some(VirtualMediaRef::new("cover_audio", "clip_cover_audio")),
            cover_range,
            AudioMix::muted(),
        );

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(25)),
        );

        let a1 = decode_plan
            .audio
            .iter()
            .find(|request| request.channel == AudioChannel::A1)
            .unwrap();
        let a2 = decode_plan
            .audio
            .iter()
            .find(|request| request.channel == AudioChannel::A2)
            .unwrap();

        assert_eq!(a1.mix, AudioMix::with_gain_db_tenths(-60));
        assert_eq!(a1.source_frame, decode_plan.source_frame);
        assert_eq!(a1.pts_sec, decode_plan.pts_sec);
        assert_eq!(a1.sample_span, AudioSampleSpan::new(48_000, 49_920));
        assert!(a2.mix.is_muted());
        assert_eq!(a2.source_frame, decode_plan.source_frame);
        assert_eq!(a2.pts_sec, decode_plan.pts_sec);
        assert_eq!(a2.sample_span, a1.sample_span);
    }

    #[test]
    fn null_backend_rejects_any_filmstrip_decode_input() {
        let mut backend = NullBroadcastBackend;
        let bad_plan = FrameDecodePlan {
            source_frame: FrameNumber(0),
            pts_sec: 0.0,
            video: vec![VideoDecodeRequest {
                layer_id: "filmstrip:source".into(),
                role: VideoRenderRole::Base,
                z_priority: ZPriority::BASE_VIDEO,
                source_frame: FrameNumber(0),
                pts_sec: 0.0,
                media_seek_sec: 0.0,
                source: VideoRenderSource::VirtualShot {
                    virtual_shot_id: "shot".into(),
                    clip_id: "clip".into(),
                },
            }],
            audio: vec![],
            markers: vec![],
            effects: vec![],
        };

        let err = backend.decode_frame(&bad_plan).unwrap_err();
        assert!(err.message.contains("filmstrip"));
    }

    #[test]
    fn null_backend_accepts_valid_broadcast_decode_plan() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source());
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(100)),
        );
        let mut backend = NullBroadcastBackend;

        let decoded = backend.decode_frame(&decode_plan).unwrap();
        assert_eq!(decoded.source_frame, FrameNumber(100));
        assert_eq!(decoded.video.len(), 1);
        assert_eq!(decoded.audio.len(), 2);
    }

    #[test]
    fn decoded_frame_validation_rejects_wrong_carrier_frame() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source());
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(100)),
        );
        let mut backend = NullBroadcastBackend;
        let mut decoded = backend.decode_frame(&decode_plan).unwrap();
        decoded.source_frame = FrameNumber(101);

        let err = decoded.validate_against_plan(&decode_plan).unwrap_err();
        assert!(err.message.contains("does not match requested frame"));
    }

    #[test]
    fn decoded_frame_validation_rejects_audio_pts_mismatch() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source());
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(125)),
        );
        let mut backend = NullBroadcastBackend;
        let mut decoded = backend.decode_frame(&decode_plan).unwrap();
        decoded.audio[0].pts_sec += 0.04;

        let err = decoded.validate_against_plan(&decode_plan).unwrap_err();
        assert!(err.message.contains("decoded audio bus"));
        assert!(err.message.contains("expected"));
    }

    #[test]
    fn decoded_frame_validation_rejects_audio_sample_span_mismatch() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source());
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(125)),
        );
        let mut backend = NullBroadcastBackend;
        let mut decoded = backend.decode_frame(&decode_plan).unwrap();
        decoded.audio[0].sample_span = AudioSampleSpan::new(0, 1_920);

        let err = decoded.validate_against_plan(&decode_plan).unwrap_err();
        assert!(err.message.contains("sample span"));
    }

    #[test]
    fn decoded_frame_validation_rejects_media_seek_mismatch() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source());
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(125)),
        );
        let mut backend = NullBroadcastBackend;
        let mut decoded = backend.decode_frame(&decode_plan).unwrap();
        decoded.video[0].media_seek_sec = 1.0;

        let err = decoded.validate_against_plan(&decode_plan).unwrap_err();
        assert!(err.message.contains("media seek"));
    }

    #[test]
    fn decoded_frame_validation_rejects_unrequested_video_layer() {
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source());
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(100)),
        );
        let mut backend = NullBroadcastBackend;
        let mut decoded = backend.decode_frame(&decode_plan).unwrap();
        decoded.video[0].layer_id = "unexpected".into();

        let err = decoded.validate_against_plan(&decode_plan).unwrap_err();
        assert!(err.message.contains("was not requested"));
    }
}
