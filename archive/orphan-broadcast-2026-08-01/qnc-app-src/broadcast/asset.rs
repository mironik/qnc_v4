//! Media asset resolution for broadcast decode plans.
//!
//! The timeline/DB side talks in virtual shots and clip IDs. A media backend
//! must not guess paths, URLs, or fallback to filmstrip images. This module
//! resolves each real decode request into an explicit media asset or an
//! explicit blank/silence source.

use std::path::PathBuf;

use super::backend::{
    AudioDecodeRequest, DecodeEffectEvent, DecodeError, DecodeMarkerEvent, DecodedProgramFrame,
    FrameDecodePlan, VideoDecodeRequest,
};
use super::render::{AudioRenderSource, VideoRenderSource};
use super::timebase::{FrameNumber, Timebase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastMediaKind {
    Proxy,
    Original,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastMediaLocation {
    LocalPath(PathBuf),
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastMediaAsset {
    pub project_id: String,
    pub virtual_shot_id: String,
    pub clip_id: String,
    pub kind: BroadcastMediaKind,
    pub location: BroadcastMediaLocation,
    pub source_timebase: Timebase,
    pub has_video: bool,
    pub has_audio: bool,
    pub audio_channels: u8,
    /// Container audio stream count. When equal to `audio_channels` and >1,
    /// buses map with `-map 0:a:N`; otherwise one multi-channel stream uses pan.
    pub audio_stream_count: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastMediaAssetSeed {
    pub project_id: String,
    pub virtual_shot_id: String,
    pub clip_id: String,
    pub kind: BroadcastMediaKind,
    pub location: BroadcastMediaLocation,
}

impl BroadcastMediaAssetSeed {
    pub fn proxy_local(
        project_id: impl Into<String>,
        virtual_shot_id: impl Into<String>,
        clip_id: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            virtual_shot_id: virtual_shot_id.into(),
            clip_id: clip_id.into(),
            kind: BroadcastMediaKind::Proxy,
            location: BroadcastMediaLocation::LocalPath(path.into()),
        }
    }

    pub fn proxy_url(
        project_id: impl Into<String>,
        virtual_shot_id: impl Into<String>,
        clip_id: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            virtual_shot_id: virtual_shot_id.into(),
            clip_id: clip_id.into(),
            kind: BroadcastMediaKind::Proxy,
            location: BroadcastMediaLocation::Url(url.into()),
        }
    }

    pub fn with_probe_report(
        self,
        report: super::probe::BroadcastMediaProbeReport,
    ) -> BroadcastMediaAsset {
        BroadcastMediaAsset {
            project_id: self.project_id,
            virtual_shot_id: self.virtual_shot_id,
            clip_id: self.clip_id,
            kind: self.kind,
            location: self.location,
            source_timebase: report.source_timebase,
            has_video: report.has_video,
            has_audio: report.has_audio,
            audio_channels: report.audio_channels,
            audio_stream_count: report.audio_stream_count,
        }
    }
}

impl BroadcastMediaAsset {
    pub fn proxy_local(
        project_id: impl Into<String>,
        virtual_shot_id: impl Into<String>,
        clip_id: impl Into<String>,
        path: impl Into<PathBuf>,
        source_timebase: Timebase,
        has_video: bool,
        has_audio: bool,
    ) -> Self {
        Self::from_parts(
            project_id,
            virtual_shot_id,
            clip_id,
            BroadcastMediaKind::Proxy,
            BroadcastMediaLocation::LocalPath(path.into()),
            source_timebase,
            has_video,
            has_audio,
            if has_audio { 2 } else { 0 },
        )
    }

    pub fn proxy_url(
        project_id: impl Into<String>,
        virtual_shot_id: impl Into<String>,
        clip_id: impl Into<String>,
        url: impl Into<String>,
        source_timebase: Timebase,
        has_video: bool,
        has_audio: bool,
    ) -> Self {
        Self::from_parts(
            project_id,
            virtual_shot_id,
            clip_id,
            BroadcastMediaKind::Proxy,
            BroadcastMediaLocation::Url(url.into()),
            source_timebase,
            has_video,
            has_audio,
            if has_audio { 2 } else { 0 },
        )
    }

    pub fn from_parts(
        project_id: impl Into<String>,
        virtual_shot_id: impl Into<String>,
        clip_id: impl Into<String>,
        kind: BroadcastMediaKind,
        location: BroadcastMediaLocation,
        source_timebase: Timebase,
        has_video: bool,
        has_audio: bool,
        audio_channels: u8,
    ) -> Self {
        let audio_channels = if has_audio {
            audio_channels.clamp(1, 4)
        } else {
            0
        };
        Self {
            project_id: project_id.into(),
            virtual_shot_id: virtual_shot_id.into(),
            clip_id: clip_id.into(),
            kind,
            location,
            source_timebase,
            has_video,
            has_audio,
            audio_channels,
            // Unknown layout defaults to one stream (stereo/multi pan extract).
            audio_stream_count: if audio_channels > 0 { 1 } else { 0 },
        }
    }

    pub fn with_probe_report(mut self, report: super::probe::BroadcastMediaProbeReport) -> Self {
        self.source_timebase = report.source_timebase;
        self.has_video = report.has_video;
        self.has_audio = report.has_audio;
        self.audio_channels = report.audio_channels;
        self.audio_stream_count = report.audio_stream_count;
        self
    }

    pub fn uses_discrete_mono_streams(&self) -> bool {
        self.audio_stream_count > 1 && self.audio_stream_count == self.audio_channels
    }

    pub fn can_serve(self: &Self, requirement: MediaRequirement) -> bool {
        match requirement {
            MediaRequirement::Video => self.has_video,
            MediaRequirement::Audio => self.has_audio && self.audio_channels > 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaRequirement {
    Video,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaResolveError {
    pub message: String,
}

impl MediaResolveError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub trait BroadcastMediaResolver {
    fn resolve_virtual_shot(
        &self,
        virtual_shot_id: &str,
        clip_id: &str,
        requirement: MediaRequirement,
    ) -> Result<BroadcastMediaAsset, MediaResolveError>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedVideoSource {
    Media(BroadcastMediaAsset),
    Blank,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedAudioSource {
    Media(BroadcastMediaAsset),
    Silence,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedVideoDecodeRequest {
    pub request: VideoDecodeRequest,
    pub resolved_source: ResolvedVideoSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAudioDecodeRequest {
    pub request: AudioDecodeRequest,
    pub resolved_source: ResolvedAudioSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedFrameDecodePlan {
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub video: Vec<ResolvedVideoDecodeRequest>,
    pub audio: Vec<ResolvedAudioDecodeRequest>,
    pub markers: Vec<DecodeMarkerEvent>,
    pub effects: Vec<DecodeEffectEvent>,
}

impl ResolvedFrameDecodePlan {
    pub fn resolve(
        plan: &FrameDecodePlan,
        resolver: &impl BroadcastMediaResolver,
    ) -> Result<Self, MediaResolveError> {
        if plan.has_filmstrip_decode_input() {
            return Err(MediaResolveError::new(
                "filmstrip is not a resolvable playback media input",
            ));
        }

        let mut video = Vec::with_capacity(plan.video.len());
        for request in &plan.video {
            let resolved_source = match &request.source {
                VideoRenderSource::VirtualShot {
                    virtual_shot_id,
                    clip_id,
                } => ResolvedVideoSource::Media(resolver.resolve_virtual_shot(
                    virtual_shot_id,
                    clip_id,
                    MediaRequirement::Video,
                )?),
                VideoRenderSource::Blank => ResolvedVideoSource::Blank,
            };
            video.push(ResolvedVideoDecodeRequest {
                request: request.clone(),
                resolved_source,
            });
        }

        let mut audio = Vec::with_capacity(plan.audio.len());
        for request in &plan.audio {
            let resolved_source = match &request.source {
                AudioRenderSource::VirtualShot {
                    virtual_shot_id,
                    clip_id,
                    ..
                } => ResolvedAudioSource::Media(resolver.resolve_virtual_shot(
                    virtual_shot_id,
                    clip_id,
                    MediaRequirement::Audio,
                )?),
                AudioRenderSource::Silence => ResolvedAudioSource::Silence,
            };
            audio.push(ResolvedAudioDecodeRequest {
                request: request.clone(),
                resolved_source,
            });
        }

        Ok(Self {
            source_frame: plan.source_frame,
            pts_sec: plan.pts_sec,
            video,
            audio,
            markers: plan.markers.clone(),
            effects: plan.effects.clone(),
        })
    }

    pub fn unresolved_plan(&self) -> FrameDecodePlan {
        FrameDecodePlan {
            source_frame: self.source_frame,
            pts_sec: self.pts_sec,
            video: self
                .video
                .iter()
                .map(|request| request.request.clone())
                .collect(),
            audio: self
                .audio
                .iter()
                .map(|request| request.request.clone())
                .collect(),
            markers: self.markers.clone(),
            effects: self.effects.clone(),
        }
    }
}

pub trait BroadcastResolvedDecodeBackend {
    type VideoPayload;
    type AudioPayload;

    fn decode_resolved_frame(
        &mut self,
        plan: &ResolvedFrameDecodePlan,
    ) -> Result<DecodedProgramFrame<Self::VideoPayload, Self::AudioPayload>, DecodeError>;
}

#[derive(Debug, Default)]
pub struct NullResolvedBroadcastBackend;

impl BroadcastResolvedDecodeBackend for NullResolvedBroadcastBackend {
    type VideoPayload = ();
    type AudioPayload = ();

    fn decode_resolved_frame(
        &mut self,
        plan: &ResolvedFrameDecodePlan,
    ) -> Result<DecodedProgramFrame<Self::VideoPayload, Self::AudioPayload>, DecodeError> {
        let frame = DecodedProgramFrame {
            source_frame: plan.source_frame,
            pts_sec: plan.pts_sec,
            video: plan
                .video
                .iter()
                .map(|request| super::backend::DecodedVideoLayer {
                    layer_id: request.request.layer_id.clone(),
                    role: request.request.role,
                    source_frame: request.request.source_frame,
                    pts_sec: request.request.pts_sec,
                    media_seek_sec: request.request.media_seek_sec,
                    payload: None,
                })
                .collect(),
            audio: plan
                .audio
                .iter()
                .map(|request| super::backend::DecodedAudioBus {
                    layer_id: request.request.layer_id.clone(),
                    channel: request.request.channel,
                    mix: request.request.mix,
                    source_frame: request.request.source_frame,
                    pts_sec: request.request.pts_sec,
                    media_seek_sec: request.request.media_seek_sec,
                    sample_rate_hz: request.request.sample_rate_hz,
                    sample_span: request.request.sample_span,
                    payload: None,
                })
                .collect(),
            markers: plan.markers.clone(),
            effects: plan.effects.clone(),
        };
        frame.validate_against_plan(&plan.unresolved_plan())?;
        Ok(frame)
    }
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryMediaResolver {
    assets: Vec<BroadcastMediaAsset>,
}

impl InMemoryMediaResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_asset(mut self, asset: BroadcastMediaAsset) -> Self {
        self.assets.push(asset);
        self
    }
}

impl BroadcastMediaResolver for InMemoryMediaResolver {
    fn resolve_virtual_shot(
        &self,
        virtual_shot_id: &str,
        clip_id: &str,
        requirement: MediaRequirement,
    ) -> Result<BroadcastMediaAsset, MediaResolveError> {
        let Some(asset) = self
            .assets
            .iter()
            .find(|asset| asset.virtual_shot_id == virtual_shot_id && asset.clip_id == clip_id)
        else {
            return Err(MediaResolveError::new(format!(
                "missing media asset for virtual shot '{virtual_shot_id}' clip '{clip_id}'"
            )));
        };

        if !asset.can_serve(requirement) {
            return Err(MediaResolveError::new(format!(
                "media asset '{}'/'{}' cannot serve {:?}",
                virtual_shot_id, clip_id, requirement
            )));
        }

        Ok(asset.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::backend::{AudioDecodeRequest, FrameDecodePlan, VideoDecodeRequest};
    use crate::broadcast::layers::{AudioChannel, AudioMix, ZPriority};
    use crate::broadcast::render::{AudioRenderSource, VideoRenderRole, VideoRenderSource};
    use crate::broadcast::sync::{AudioSampleSpan, BROADCAST_AUDIO_SAMPLE_RATE_HZ};
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        BroadcastFrameScheduler, BroadcastPlaybackSource, BroadcastProgramGraph,
        BroadcastRenderPlan,
    };

    fn asset(has_video: bool, has_audio: bool) -> BroadcastMediaAsset {
        BroadcastMediaAsset::proxy_local(
            "project",
            "shot",
            "clip",
            PathBuf::from("media/proxy.mxf"),
            Timebase::from_source_fps(25.0),
            has_video,
            has_audio,
        )
    }

    fn source_plan() -> FrameDecodePlan {
        let source = BroadcastPlaybackSource {
            project_id: "project".into(),
            virtual_shot_id: "shot".into(),
            clip_id: "clip".into(),
            source_range: FrameRange::new(FrameNumber(100), FrameNumber(200)),
            source_timebase: Timebase::from_source_fps(25.0),
            has_video: true,
            has_audio: true,
            audio_channels: 2,
        };
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        FrameDecodePlan::from_scheduled(&render_plan, scheduler.schedule_frame(FrameNumber(100)))
    }

    #[test]
    fn resolved_plan_maps_virtual_shot_requests_to_media_asset() {
        let resolver = InMemoryMediaResolver::new().with_asset(asset(true, true));
        let resolved = ResolvedFrameDecodePlan::resolve(&source_plan(), &resolver).unwrap();

        assert_eq!(resolved.video.len(), 1);
        assert_eq!(resolved.audio.len(), 2);
        assert!(matches!(
            resolved.video[0].resolved_source,
            ResolvedVideoSource::Media(_)
        ));
        assert!(matches!(
            resolved.audio[0].resolved_source,
            ResolvedAudioSource::Media(_)
        ));
        assert!(
            matches!(
                resolved.audio[1].resolved_source,
                ResolvedAudioSource::Media(_)
            ),
            "stereo source maps ch1 onto A2 media — not cover silence"
        );
    }

    #[test]
    fn resolved_plan_keeps_blank_and_silence_as_explicit_non_media() {
        let plan = FrameDecodePlan {
            source_frame: FrameNumber(0),
            pts_sec: 0.0,
            video: vec![VideoDecodeRequest {
                layer_id: "video:blank".into(),
                role: VideoRenderRole::Base,
                z_priority: ZPriority::BASE_VIDEO,
                source_frame: FrameNumber(0),
                pts_sec: 0.0,
                media_seek_sec: 0.0,
                source: VideoRenderSource::Blank,
            }],
            audio: vec![AudioDecodeRequest {
                layer_id: "audio:silence".into(),
                channel: AudioChannel::A1,
                mix: AudioMix::UNITY,
                source_frame: FrameNumber(0),
                pts_sec: 0.0,
                media_seek_sec: 0.0,
                sample_rate_hz: BROADCAST_AUDIO_SAMPLE_RATE_HZ,
                sample_span: AudioSampleSpan::new(0, 1_920),
                source: AudioRenderSource::Silence,
            }],
            markers: Vec::new(),
            effects: Vec::new(),
        };
        let resolver = InMemoryMediaResolver::new();
        let resolved = ResolvedFrameDecodePlan::resolve(&plan, &resolver).unwrap();

        assert_eq!(
            resolved.video[0].resolved_source,
            ResolvedVideoSource::Blank
        );
        assert_eq!(
            resolved.audio[0].resolved_source,
            ResolvedAudioSource::Silence
        );
    }

    #[test]
    fn resolver_rejects_missing_media_asset() {
        let resolver = InMemoryMediaResolver::new();

        let err = ResolvedFrameDecodePlan::resolve(&source_plan(), &resolver).unwrap_err();

        assert!(err.message.contains("missing media asset"));
    }

    #[test]
    fn resolver_rejects_video_request_for_audio_only_asset() {
        let resolver = InMemoryMediaResolver::new().with_asset(asset(false, true));

        let err = ResolvedFrameDecodePlan::resolve(&source_plan(), &resolver).unwrap_err();

        assert!(err.message.contains("cannot serve"));
        assert!(err.message.contains("Video"));
    }

    #[test]
    fn resolved_plan_can_return_unresolved_validation_plan() {
        let resolver = InMemoryMediaResolver::new().with_asset(asset(true, true));
        let source_plan = source_plan();
        let resolved = ResolvedFrameDecodePlan::resolve(&source_plan, &resolver).unwrap();

        assert_eq!(resolved.unresolved_plan(), source_plan);
    }

    #[test]
    fn asset_seed_has_no_timebase_until_probe_report_arrives() {
        let seed = BroadcastMediaAssetSeed::proxy_url(
            "project",
            "shot",
            "clip",
            "http://127.0.0.1/media/clip",
        );
        let report = super::super::probe::BroadcastMediaProbeReport {
            source_timebase: Timebase::from_source_rate(50, 1).unwrap(),
            has_video: true,
            has_audio: true,
            audio_channels: 2,
            audio_stream_count: 1,
            video_width: Some(1920),
            video_height: Some(1080),
        };

        let asset = seed.with_probe_report(report);

        assert_eq!(
            asset.source_timebase,
            Timebase::from_source_rate(50, 1).unwrap()
        );
        assert_eq!(asset.audio_channels, 2);
        assert!(matches!(asset.location, BroadcastMediaLocation::Url(_)));
    }
}
