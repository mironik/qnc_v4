//! Story form → [`UniversalTimelineSpec`] for editorial playback.
//!
//! The broadcast player is a neutral component: it only receives a finished
//! program spec + assets. Story/Wrap mapping lives here, not in the player.

use crate::broadcast::{
    build_layered_program, BroadcastMediaAsset, BroadcastMediaProbeReport, FrameRange,
    LayeredProgramInput, MarkerKind, ProgramMarkerInput, ProgramOverlayInput, Timebase,
    UniversalTimelineSpec, VirtualMediaRef,
};
use crate::editorial::types::{StoryCover, StoryMarker, StoryPart, StoryShot};
use crate::frame_time::normalize_fps;

/// Map current Story wrap/segment snapshot into a neutral layered program.
pub fn story_wrap_program(
    project_id: &str,
    part: &StoryPart,
    covers: &[StoryCover],
    markers: &[StoryMarker],
    clips: &[StoryShot],
    timeline_fps: f64,
) -> Option<(UniversalTimelineSpec, Vec<BroadcastMediaAsset>)> {
    let fps = normalize_fps(timeline_fps);
    let timebase = Timebase::try_from_source_fps(fps).ok()?;
    let part_in = part.in_seconds.unwrap_or(0.0).max(0.0);
    let part_out = part
        .out_seconds
        .filter(|v| *v > part_in)
        .unwrap_or(part_in + 1.0);
    let carrier_range = FrameRange::new(
        timebase.frame_at_seconds(part_in),
        timebase.frame_at_seconds(part_out),
    );

    let kind = part.kind.trim().to_ascii_lowercase();
    let omit_base_video = kind == "off" || kind == "vo" || kind == "ton" || kind.contains("off");

    let base_shot = find_shot(clips, &part.virtual_shot_id, &part.clip_id);
    let base_media = {
        let vid = first_non_empty([part.virtual_shot_id.as_str(), part.clip_id.as_str()])?;
        let clip = first_non_empty([part.clip_id.as_str(), part.virtual_shot_id.as_str()])?;
        Some(VirtualMediaRef::new(vid, clip))
    };

    let mut assets = Vec::new();
    if let Some(shot) = base_shot {
        if let Some(asset) = asset_from_shot(project_id, shot, timebase) {
            assets.push(asset);
        }
    }

    let mut overlays = Vec::new();
    for (i, cover) in covers.iter().enumerate() {
        if cover.timeline_end_sec <= cover.timeline_start_sec {
            continue;
        }
        if cover.timeline_end_sec <= part_in || cover.timeline_start_sec >= part_out {
            continue;
        }
        let start = timebase.frame_at_seconds(cover.timeline_start_sec.max(part_in));
        let end = timebase.frame_at_seconds(cover.timeline_end_sec.min(part_out));
        if end.0 <= start.0 {
            continue;
        }
        let vid = first_non_empty([cover.virtual_shot_id.as_str(), cover.clip_id.as_str()])
            .unwrap_or_else(|| format!("cover_{}", cover.cover_id));
        let clip = first_non_empty([cover.clip_id.as_str(), cover.virtual_shot_id.as_str()])
            .unwrap_or_else(|| vid.clone());
        let media = VirtualMediaRef::new(vid, clip);
        if let Some(shot) = find_shot(clips, &cover.virtual_shot_id, &cover.clip_id) {
            if let Some(asset) = asset_from_shot(project_id, shot, timebase) {
                assets.push(asset);
            }
        }
        overlays.push(ProgramOverlayInput {
            overlay_index: (i as u8).saturating_add(1),
            video: media.clone(),
            audio: Some(media),
            frame_range: FrameRange::new(start, end),
        });
    }

    let mut marker_inputs = Vec::new();
    for marker in markers {
        if !marker.part_id.is_empty() && marker.part_id != part.part_id {
            continue;
        }
        let frame = timebase.frame_at_seconds(marker.timeline_sec.max(0.0));
        if frame.0 < carrier_range.start.0 || frame.0 > carrier_range.end_exclusive.0 {
            continue;
        }
        let kind = match marker.label.trim().to_ascii_lowercase().as_str() {
            "in" => MarkerKind::In,
            "out" => MarkerKind::Out,
            _ => MarkerKind::M,
        };
        marker_inputs.push(ProgramMarkerInput {
            marker_id: if marker.marker_id.is_empty() {
                format!("m{}", marker.timeline_sec)
            } else {
                marker.marker_id.clone()
            },
            kind,
            frame,
        });
    }

    let has_base_audio = base_shot.map(|s| s.has_audio).unwrap_or(true);
    let spec = build_layered_program(LayeredProgramInput {
        project_id: project_id.to_string(),
        program_id: part.part_id.clone(),
        clip_id: part.clip_id.clone(),
        timebase,
        carrier_range,
        omit_base_video,
        force_blank_base: false,
        base_media,
        has_base_audio,
        overlays,
        markers: marker_inputs,
    });

    Some((spec, assets))
}

fn find_shot<'a>(
    clips: &'a [StoryShot],
    virtual_shot_id: &str,
    clip_id: &str,
) -> Option<&'a StoryShot> {
    clips.iter().find(|s| {
        (!virtual_shot_id.is_empty()
            && (s.shot_id == virtual_shot_id || s.root_shot_id == virtual_shot_id))
            || (!clip_id.is_empty() && s.clip_id == clip_id)
    })
}

fn asset_from_shot(
    project_id: &str,
    shot: &StoryShot,
    timebase: Timebase,
) -> Option<BroadcastMediaAsset> {
    let path = shot.play_path.trim();
    if path.is_empty() {
        return None;
    }
    let vid = first_non_empty([
        shot.shot_id.as_str(),
        shot.root_shot_id.as_str(),
        shot.clip_id.as_str(),
    ])?;
    let clip = first_non_empty([shot.clip_id.as_str(), shot.shot_id.as_str()])?;
    let seed = crate::broadcast::BroadcastMediaAssetSeed::proxy_local(
        project_id,
        vid,
        clip,
        std::path::PathBuf::from(path),
    );
    Some(seed.with_probe_report(BroadcastMediaProbeReport {
        source_timebase: timebase,
        has_video: true,
        has_audio: shot.has_audio,
        audio_channels: if shot.has_audio {
            shot.audio_channels.max(2).min(4)
        } else {
            0
        },
        audio_stream_count: if shot.has_audio { 1 } else { 0 },
        video_width: None,
        video_height: None,
    }))
}

fn first_non_empty<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<String> {
    values
        .into_iter()
        .map(str::trim)
        .find(|v| !v.is_empty())
        .map(|v| v.to_string())
}
