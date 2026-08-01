//! Broadcast audio mix decision plan.
//!
//! Audio is not part of the visual Z axis. It is a time-aligned bus on the
//! same celluloid carrier. This module converts active audio decode requests
//! for one carrier frame into explicit mix inputs for the audio backend.

use super::backend::FrameDecodePlan;
use super::layers::{AudioChannel, AudioMix};
use super::render::AudioRenderSource;
use super::timebase::FrameNumber;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBusRole {
    OffVoTon,
    Cover,
    Auxiliary(AudioChannel),
}

impl AudioBusRole {
    pub fn from_channel(channel: AudioChannel) -> Self {
        match channel {
            AudioChannel::A1 => Self::OffVoTon,
            AudioChannel::A2 => Self::Cover,
            other => Self::Auxiliary(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioMixInput {
    pub layer_id: String,
    pub channel: AudioChannel,
    pub role: AudioBusRole,
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub mix: AudioMix,
    pub source: AudioRenderSource,
}

impl AudioMixInput {
    pub fn is_audible(&self) -> bool {
        self.mix.is_audible()
    }

    pub fn effective_linear_gain(&self) -> f32 {
        self.mix.effective_linear_gain()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioMixPlan {
    pub source_frame: FrameNumber,
    pub pts_sec: f64,
    pub inputs: Vec<AudioMixInput>,
}

impl AudioMixPlan {
    pub fn try_from_decode_plan(plan: &FrameDecodePlan) -> Result<Self, AudioMixPlanError> {
        for request in &plan.audio {
            if request.source_frame != plan.source_frame {
                return Err(AudioMixPlanError::new(format!(
                    "audio request '{}' is on frame {:?}, expected {:?}",
                    request.layer_id, request.source_frame, plan.source_frame
                )));
            }
            if request.pts_sec != plan.pts_sec {
                return Err(AudioMixPlanError::new(format!(
                    "audio request '{}' is on PTS {}, expected {}",
                    request.layer_id, request.pts_sec, plan.pts_sec
                )));
            }
        }

        Ok(Self {
            source_frame: plan.source_frame,
            pts_sec: plan.pts_sec,
            inputs: plan
                .audio
                .iter()
                .map(|request| AudioMixInput {
                    layer_id: request.layer_id.clone(),
                    channel: request.channel,
                    role: AudioBusRole::from_channel(request.channel),
                    source_frame: request.source_frame,
                    pts_sec: request.pts_sec,
                    mix: request.mix,
                    source: request.source.clone(),
                })
                .collect(),
        })
    }

    pub fn audible_inputs(&self) -> impl Iterator<Item = &AudioMixInput> {
        self.inputs.iter().filter(|input| input.is_audible())
    }

    pub fn input_for_channel(&self, channel: AudioChannel) -> Option<&AudioMixInput> {
        self.inputs.iter().find(|input| input.channel == channel)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioMixPlanError {
    pub message: String,
}

impl AudioMixPlanError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::backend::FrameDecodePlan;
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::{
        BroadcastFrameScheduler, BroadcastProgramGraph, BroadcastRenderPlan, CelluloidTrack,
        UniversalTimelineSpec, VirtualMediaRef,
    };

    #[test]
    fn mix_plan_maps_a1_and_a2_roles_without_visual_z_axis() {
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
        let mix_plan = AudioMixPlan::try_from_decode_plan(&decode_plan).unwrap();

        let a1 = mix_plan.input_for_channel(AudioChannel::A1).unwrap();
        let a2 = mix_plan.input_for_channel(AudioChannel::A2).unwrap();

        assert_eq!(a1.role, AudioBusRole::OffVoTon);
        assert_eq!(a1.mix.gain_db_tenths(), -60);
        assert!(a1.is_audible());
        assert_eq!(a2.role, AudioBusRole::Cover);
        assert!(a2.mix.is_muted());
        assert_eq!(
            mix_plan
                .audible_inputs()
                .map(|input| input.channel)
                .collect::<Vec<_>>(),
            vec![AudioChannel::A1]
        );
    }

    #[test]
    fn mix_plan_rejects_audio_not_on_carrier_frame() {
        let carrier = CelluloidTrack::new(
            "project",
            "wrap_vo",
            "timeline",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(0), FrameNumber(100)),
        );
        let mut spec = UniversalTimelineSpec::new(carrier);
        spec.add_off_vo_audio(VirtualMediaRef::new("vo", "clip_vo"));

        let graph = BroadcastProgramGraph::from_universal_timeline(spec);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let mut decode_plan = FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(25)),
        );
        decode_plan.audio[0].source_frame = FrameNumber(24);

        let err = AudioMixPlan::try_from_decode_plan(&decode_plan).unwrap_err();
        assert!(err.message.contains("expected"));
    }
}
