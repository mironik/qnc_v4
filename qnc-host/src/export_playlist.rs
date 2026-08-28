//! Export-flat playlist builder.
//!
//! This module is the neutral bridge between the editorial montage playlist and
//! any full-resolution consumer. It does not enqueue jobs, render media, or know
//! about UI forms. Callers get one flat, frame-based playlist linked to original
//! media paths.

use std::collections::HashMap;
use std::path::PathBuf;

use qnc_service_contracts::{
    ExportHiResJobPayload, ExportHiResPlaylistItem, ExportHiResPlaylistSource, FrameTimebase,
    MediaAccessKind, MediaLocator, MediaResolveRequest,
};

use crate::frame_time::{is_valid_fps, rational_fps};
use crate::media::ProjectMediaGateway;

use crate::editorial_playlist::{
    EditorialCover, EditorialPlaylist, EditorialSegment, SourceTimebase,
};

const PROGRAM_AUDIO_OUTPUT_CH1: u8 = 0;
const PROGRAM_AUDIO_OUTPUT_CH2: u8 = 1;

#[derive(Debug, Clone)]
pub(crate) struct ExportFlatPlaylist {
    pub timeline_timebase: FrameTimebase,
    pub duration_frames: i64,
    pub items: Vec<ExportHiResPlaylistItem>,
}

#[derive(Debug, Clone)]
struct ResolvedOriginal {
    path: PathBuf,
    has_audio: bool,
}

pub(crate) fn build_export_flat_playlist(
    media_gateway: &ProjectMediaGateway,
    playlist: &EditorialPlaylist,
) -> Result<ExportFlatPlaylist, String> {
    let timeline_timebase = editorial_timeline_timebase(playlist)?;
    let mut resolver = OriginalResolver::new(media_gateway, &playlist.project_id);
    let mut items = Vec::new();
    for segment in &playlist.segments {
        items.extend(flat_items_for_segment(segment, &mut resolver)?);
    }
    if items.is_empty() {
        return Err("Program input je prazan; nema sto exportirati.".into());
    }
    Ok(ExportFlatPlaylist {
        timeline_timebase,
        duration_frames: playlist.duration_frames.max(0),
        items,
    })
}

pub(crate) fn materialize_export_flat_payload(
    media_gateway: &ProjectMediaGateway,
    playlist: &EditorialPlaylist,
    output_path: PathBuf,
    export_id: &str,
) -> Result<ExportHiResJobPayload, String> {
    let flat = build_export_flat_playlist(media_gateway, playlist)?;
    let output_path = output_path_for_flat_playlist(output_path, &flat)?;
    Ok(ExportHiResJobPayload {
        project_id: playlist.project_id.clone(),
        export_id: export_id.to_string(),
        output_path,
        timeline_timebase: flat.timeline_timebase,
        duration_frames: flat.duration_frames,
        items: flat.items,
    })
}

pub(crate) fn editorial_timeline_timebase(
    playlist: &EditorialPlaylist,
) -> Result<FrameTimebase, String> {
    frame_timebase_from_fps(playlist.timeline_fps, "program input")
}

fn output_path_for_flat_playlist(
    output_path: PathBuf,
    playlist: &ExportFlatPlaylist,
) -> Result<PathBuf, String> {
    if output_path.extension().is_some() {
        return Ok(output_path);
    }
    let extension = output_extension_from_items(&playlist.items)?;
    Ok(output_path.with_extension(extension))
}

fn output_extension_from_items(items: &[ExportHiResPlaylistItem]) -> Result<String, String> {
    let source = items
        .iter()
        .flat_map(|item| item.sources.iter())
        .find(|source| source.has_video)
        .ok_or_else(|| "Export HI-res nema video original za odabir containera.".to_string())?;
    let extension = source
        .original_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "Original za '{}' nema ekstenziju za HI-res container: {}",
                source.clip_id,
                source.original_path.display()
            )
        })?;
    match extension.as_str() {
        "mxf" | "mov" | "mp4" => Ok(extension),
        other => Err(format!(
            "Export HI-res container iz originala jos nije podrzan: .{other}"
        )),
    }
}

fn flat_items_for_segment(
    segment: &EditorialSegment,
    resolver: &mut OriginalResolver<'_>,
) -> Result<Vec<ExportHiResPlaylistItem>, String> {
    let clip_id = segment.clip_id.trim();
    if clip_id.is_empty() {
        return Err(format!(
            "Segment '{}' nema clip_id za HI-res export.",
            segment.part_id
        ));
    }
    let original = resolver.resolve(clip_id)?.clone();
    let is_off = segment.kind.trim().eq_ignore_ascii_case("offovi");
    let has_base_audio = is_off || original.has_audio;
    let segment_start = segment.global_start_frame.max(0);
    let segment_end = segment.global_end_frame.max(segment_start + 1);
    let mut out = Vec::new();

    let cover_ranges = cover_ranges_for_segment(&segment.covers, segment_start, segment_end);
    let mut cursor = segment_start;
    for cover in cover_ranges {
        let cover_start = cover.timeline_start_frame.max(segment_start).max(cursor);
        let cover_end = cover.timeline_end_frame.min(segment_end).max(cover_start);
        if cover_end <= cover_start {
            continue;
        }
        if cursor < cover_start {
            out.push(base_item_for_segment(
                segment,
                &original,
                has_base_audio,
                is_off,
                cursor,
                cover_start,
            )?);
        }

        let mut sources = Vec::new();
        if has_base_audio {
            sources.push(segment_source_for_range(
                segment,
                &original,
                "base_audio",
                false,
                true,
                Some(PROGRAM_AUDIO_OUTPUT_CH1),
                cover_start,
                cover_end,
            )?);
        }
        sources.push(cover_source_for_range(
            cover,
            resolver,
            cover_start,
            cover_end,
        )?);
        out.push(ExportHiResPlaylistItem {
            item_id: item_id_for_range(cover_start, cover_end),
            record_in_frame: cover_start,
            record_out_frame: cover_end,
            sources,
        });
        cursor = cover_end;
    }

    if cursor < segment_end {
        out.push(base_item_for_segment(
            segment,
            &original,
            has_base_audio,
            is_off,
            cursor,
            segment_end,
        )?);
    }

    Ok(out)
}

fn base_item_for_segment(
    segment: &EditorialSegment,
    original: &ResolvedOriginal,
    has_base_audio: bool,
    is_off: bool,
    record_in_frame: i64,
    record_out_frame: i64,
) -> Result<ExportHiResPlaylistItem, String> {
    let mut sources = Vec::new();
    if !is_off {
        sources.push(segment_source_for_range(
            segment,
            original,
            "base_video",
            true,
            false,
            None,
            record_in_frame,
            record_out_frame,
        )?);
    }
    if has_base_audio {
        sources.push(segment_source_for_range(
            segment,
            original,
            "base_audio",
            false,
            true,
            Some(PROGRAM_AUDIO_OUTPUT_CH1),
            record_in_frame,
            record_out_frame,
        )?);
    }
    Ok(ExportHiResPlaylistItem {
        item_id: item_id_for_range(record_in_frame, record_out_frame),
        record_in_frame,
        record_out_frame,
        sources,
    })
}

fn segment_source_for_range(
    segment: &EditorialSegment,
    original: &ResolvedOriginal,
    source_kind: &str,
    has_video: bool,
    has_audio: bool,
    audio_output_channel: Option<u8>,
    record_in_frame: i64,
    record_out_frame: i64,
) -> Result<ExportHiResPlaylistSource, String> {
    let (source_in_frame, source_out_frame) = source_range_for_record_chunk(
        segment.source_in_frame,
        segment.source_out_frame,
        segment.global_start_frame,
        record_in_frame,
        record_out_frame,
    );
    Ok(ExportHiResPlaylistSource {
        source_id: format!("part:{}:{source_kind}", segment.part_id),
        source_kind: source_kind.to_string(),
        clip_id: segment.clip_id.trim().to_string(),
        virtual_shot_id: segment.virtual_shot_id.clone(),
        original_path: original.path.clone(),
        source_in_frame,
        source_out_frame,
        source_timebase: frame_timebase_from_source(segment.source_timebase, &segment.clip_id)?,
        has_video,
        has_audio,
        audio_output_channel,
    })
}

fn cover_source_for_range(
    cover: &EditorialCover,
    resolver: &mut OriginalResolver<'_>,
    record_in_frame: i64,
    record_out_frame: i64,
) -> Result<ExportHiResPlaylistSource, String> {
    let clip_id = cover.clip_id.trim();
    if clip_id.is_empty() {
        return Err(format!(
            "Pokrivalica '{}' nema clip_id za HI-res export.",
            cover.cover_id
        ));
    }
    let original = resolver.resolve(clip_id)?;
    let (source_in_frame, source_out_frame) = source_range_for_record_chunk(
        cover.source_in_frame,
        cover.source_out_frame,
        cover.timeline_start_frame,
        record_in_frame,
        record_out_frame,
    );
    Ok(ExportHiResPlaylistSource {
        source_id: format!("cover:{}", cover.cover_id),
        source_kind: "cover".into(),
        clip_id: clip_id.to_string(),
        virtual_shot_id: cover.virtual_shot_id.clone(),
        original_path: original.path.clone(),
        source_in_frame,
        source_out_frame,
        source_timebase: frame_timebase_from_source(cover.source_timebase, clip_id)?,
        has_video: true,
        has_audio: original.has_audio,
        audio_output_channel: original.has_audio.then_some(PROGRAM_AUDIO_OUTPUT_CH2),
    })
}

fn cover_ranges_for_segment<'a>(
    covers: &'a [EditorialCover],
    segment_start: i64,
    segment_end: i64,
) -> Vec<&'a EditorialCover> {
    let mut ranges = covers
        .iter()
        .filter(|cover| {
            cover.streamable
                && cover.timeline_end_frame > segment_start
                && cover.timeline_start_frame < segment_end
                && cover.timeline_end_frame > cover.timeline_start_frame
        })
        .collect::<Vec<_>>();
    ranges.sort_by_key(|cover| {
        (
            cover.timeline_start_frame,
            cover.timeline_end_frame,
            cover.cover_id.clone(),
        )
    });
    ranges
}

fn source_range_for_record_chunk(
    source_in_frame: i64,
    source_out_frame: i64,
    origin_record_in_frame: i64,
    record_in_frame: i64,
    record_out_frame: i64,
) -> (i64, i64) {
    let source_in = source_in_frame.max(0);
    let source_out = source_out_frame.max(source_in + 1);
    let offset = record_in_frame
        .max(origin_record_in_frame)
        .saturating_sub(origin_record_in_frame.max(0));
    let start = source_in.saturating_add(offset).min(source_out - 1);
    let span = record_out_frame
        .max(record_in_frame + 1)
        .saturating_sub(record_in_frame.max(0))
        .max(1);
    let end = start.saturating_add(span).min(source_out).max(start + 1);
    (start, end)
}

fn frame_timebase_from_source(
    timebase: SourceTimebase,
    label: &str,
) -> Result<FrameTimebase, String> {
    if timebase.fps_num <= 0 || timebase.fps_den <= 0 {
        return Err(format!(
            "'{label}' nema valjan originalni timebase iz probea."
        ));
    }
    FrameTimebase::new(timebase.fps_num as u32, timebase.fps_den as u32)
        .map_err(|error| error.message)
}

fn frame_timebase_from_fps(fps: f64, label: &str) -> Result<FrameTimebase, String> {
    if !is_valid_fps(fps) {
        return Err(format!("{label} nema valjan source FPS iz probea."));
    }
    let (fps_num, fps_den) = rational_fps(fps);
    if fps_num <= 0 || fps_den <= 0 {
        return Err(format!("{label} nema valjan source timebase iz probea."));
    }
    FrameTimebase::new(fps_num as u32, fps_den as u32).map_err(|error| error.message)
}

fn item_id_for_range(record_in_frame: i64, record_out_frame: i64) -> String {
    format!(
        "item:{}-{}",
        record_in_frame.max(0),
        record_out_frame.max(record_in_frame + 1)
    )
}

struct OriginalResolver<'a> {
    gateway: &'a ProjectMediaGateway,
    project_id: &'a str,
    cache: HashMap<String, ResolvedOriginal>,
}

impl<'a> OriginalResolver<'a> {
    fn new(gateway: &'a ProjectMediaGateway, project_id: &'a str) -> Self {
        Self {
            gateway,
            project_id,
            cache: HashMap::new(),
        }
    }

    fn resolve(&mut self, clip_id: &str) -> Result<&ResolvedOriginal, String> {
        let clip_id = clip_id.trim();
        if clip_id.is_empty() {
            return Err("clip_id required".into());
        }
        if !self.cache.contains_key(clip_id) {
            let resolved = self.resolve_uncached(clip_id)?;
            self.cache.insert(clip_id.to_string(), resolved);
        }
        self.cache
            .get(clip_id)
            .ok_or_else(|| format!("Original nije rijesen za clip '{clip_id}'"))
    }

    fn resolve_uncached(&self, clip_id: &str) -> Result<ResolvedOriginal, String> {
        let response = self
            .gateway
            .resolve_sync(MediaResolveRequest {
                project_id: self.project_id.to_string(),
                clip_id: clip_id.to_string(),
                access: MediaAccessKind::OriginalMaster,
                fallback: None,
            })
            .map_err(|error| error.message)?;
        let path = match response.media.locator {
            MediaLocator::LocalPath { path } => path,
            MediaLocator::IntranetPath { uri } => {
                return Err(format!(
                    "Original za '{clip_id}' nije lokalni/shared path za ovog workera: {uri}"
                ));
            }
            MediaLocator::ManagedAsset { asset_id } => {
                return Err(format!(
                    "Original za '{clip_id}' je managed asset bez lokalnog worker patha: {asset_id}"
                ));
            }
        };
        if !path.is_file() {
            return Err(format!(
                "Original za '{clip_id}' ne postoji na disku: {}",
                path.display()
            ));
        }
        let has_audio = response
            .metadata
            .get("has_audio")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        Ok(ResolvedOriginal { path, has_audio })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video_source(path: &str) -> ExportHiResPlaylistSource {
        ExportHiResPlaylistSource {
            source_id: "part:p1:base_video".into(),
            source_kind: "base_video".into(),
            clip_id: "base_clip".into(),
            virtual_shot_id: "base_shot".into(),
            original_path: PathBuf::from(path),
            source_in_frame: 0,
            source_out_frame: 50,
            source_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            has_video: true,
            has_audio: false,
            audio_output_channel: None,
        }
    }

    #[test]
    fn output_path_derives_extension_from_original_video_when_pending_has_no_extension() {
        let playlist = ExportFlatPlaylist {
            timeline_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            duration_frames: 50,
            items: vec![ExportHiResPlaylistItem {
                item_id: "item:0-50".into(),
                record_in_frame: 0,
                record_out_frame: 50,
                sources: vec![video_source("C:/card/ClipA.MXF")],
            }],
        };

        let output = output_path_for_flat_playlist(PathBuf::from("C:/out/master"), &playlist)
            .expect("output extension");

        assert_eq!(output, PathBuf::from("C:/out/master.mxf"));
    }

    #[test]
    fn output_path_keeps_existing_transport_extension() {
        let playlist = ExportFlatPlaylist {
            timeline_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            duration_frames: 50,
            items: vec![ExportHiResPlaylistItem {
                item_id: "item:0-50".into(),
                record_in_frame: 0,
                record_out_frame: 50,
                sources: vec![video_source("C:/card/ClipA.MXF")],
            }],
        };

        let output =
            output_path_for_flat_playlist(PathBuf::from("C:/stream/stream.m3u8"), &playlist)
                .expect("stream path");

        assert_eq!(output, PathBuf::from("C:/stream/stream.m3u8"));
    }

    #[test]
    fn flat_item_contract_keeps_cover_video_and_audio_buses() {
        let base = PathBuf::from("C:/card/base.mxf");
        let cover = PathBuf::from("C:/card/cover.mxf");
        let item = ExportHiResPlaylistItem {
            item_id: "item:cover".into(),
            record_in_frame: 50,
            record_out_frame: 100,
            sources: vec![
                ExportHiResPlaylistSource {
                    source_id: "part:p1:base_video".into(),
                    source_kind: "base_video".into(),
                    clip_id: "base_clip".into(),
                    virtual_shot_id: "base_shot".into(),
                    original_path: base.clone(),
                    source_in_frame: 10,
                    source_out_frame: 60,
                    source_timebase: FrameTimebase {
                        fps_num: 50,
                        fps_den: 1,
                    },
                    has_video: true,
                    has_audio: false,
                    audio_output_channel: None,
                },
                ExportHiResPlaylistSource {
                    source_id: "part:p1:base_audio".into(),
                    source_kind: "base_audio".into(),
                    clip_id: "base_clip".into(),
                    virtual_shot_id: "base_shot".into(),
                    original_path: base,
                    source_in_frame: 10,
                    source_out_frame: 60,
                    source_timebase: FrameTimebase {
                        fps_num: 50,
                        fps_den: 1,
                    },
                    has_video: false,
                    has_audio: true,
                    audio_output_channel: Some(0),
                },
                ExportHiResPlaylistSource {
                    source_id: "cover:c1".into(),
                    source_kind: "cover".into(),
                    clip_id: "cover_clip".into(),
                    virtual_shot_id: "cover_shot".into(),
                    original_path: cover,
                    source_in_frame: 100,
                    source_out_frame: 150,
                    source_timebase: FrameTimebase {
                        fps_num: 50,
                        fps_den: 1,
                    },
                    has_video: true,
                    has_audio: true,
                    audio_output_channel: Some(1),
                },
            ],
        };

        let video = item
            .sources
            .iter()
            .filter(|source| source.has_video)
            .last()
            .unwrap();
        let a1 = item
            .sources
            .iter()
            .filter(|source| source.has_audio && source.audio_output_channel == Some(0))
            .last()
            .unwrap();
        let a2 = item
            .sources
            .iter()
            .filter(|source| source.has_audio && source.audio_output_channel == Some(1))
            .last()
            .unwrap();

        assert_eq!(video.clip_id, "cover_clip");
        assert_eq!(a1.clip_id, "base_clip");
        assert_eq!(a2.clip_id, "cover_clip");
    }

    #[test]
    fn source_range_uses_record_frame_offset_without_seconds() {
        assert_eq!(
            source_range_for_record_chunk(100, 250, 40, 70, 95),
            (130, 155)
        );
    }
}
