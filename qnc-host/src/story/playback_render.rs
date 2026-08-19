//! F2 — server-side program audio (A1/A2 dual-mono) i preview frame.
//! Play media path: proxy-only via `media::resolve_play_media` (see docs/qnc-playback-engine.md).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::editor_assets::{ensure_virtual_stream_cached_kind, VirtualStreamKind};
use crate::frame_time::{frame_to_seconds, is_valid_fps, seconds_to_frame};
use crate::ingest::thumb::{extract_preview_jpeg_at_seek, media_has_audio_stream, resolve_ffmpeg};
use crate::media::resolve_play_media;
use crate::project::db::ProjectPaths;

use super::db::{cover_stream_frames, part_stream_frames};
use super::playback::{
    find_cover_frame, find_segment_frame, resolve_active_layer_frame_public,
    source_offset_for_record_frame, ActiveLayerKind, PlaybackSession,
};

const EPS: f64 = 0.001;
const MAX_MIX_DURATION_SEC: f64 = 120.0;

#[derive(Debug, Clone)]
pub(crate) struct MixSlice {
    pub(crate) part_id: String,
    pub(crate) duration_sec: f64,
    pub(crate) part_local_in_sec: f64,
    pub(crate) cover_id: Option<String>,
    pub(crate) cover_source_in_sec: f64,
}

pub async fn render_mixed_audio(
    paths: &ProjectPaths,
    session: &PlaybackSession,
    from_sec: f64,
    duration_sec: f64,
) -> Result<PathBuf, String> {
    if let Some(src) = session.source_clip.as_ref() {
        let from = from_sec.max(src.in_sec).min(src.out_sec.max(src.in_sec));
        let dur = duration_sec
            .max(0.25)
            .min(MAX_MIX_DURATION_SEC)
            .min((src.out_sec - from).max(0.0));
        if dur <= EPS {
            return Err("playback audio: nema trajanja za source clip".into());
        }
        let sid = session.session_id.clone();
        let pid = session.project_id.clone();
        let clip_id = src.clip_id.clone();
        let paths = paths.clone();
        return tokio::task::spawn_blocking(move || {
            render_source_audio_blocking(&paths, &sid, &pid, &clip_id, from, dur)
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    let from = from_sec.max(0.0);
    let dur = duration_sec
        .max(0.25)
        .min(MAX_MIX_DURATION_SEC)
        .min((session.playlist.duration_sec - from).max(0.0));
    if dur <= EPS {
        return Err("playback audio: nema trajanja za mix".into());
    }
    let slices = plan_mix_slices(session, from, dur);
    if slices.is_empty() {
        return Err("playback audio: prazan mix plan".into());
    }
    let sid = session.session_id.clone();
    let pid = session.project_id.clone();
    let paths = paths.clone();
    tokio::task::spawn_blocking(move || render_mix_blocking(&paths, &sid, &pid, from, dur, &slices))
        .await
        .map_err(|e| e.to_string())?
}

pub async fn render_preview_frame_at_frame(
    paths: &ProjectPaths,
    session: &PlaybackSession,
    virtual_frame: i64,
) -> Result<PathBuf, String> {
    if let Some(src) = session.source_clip.as_ref() {
        let source_frame = virtual_frame
            .max(0)
            .clamp(src.in_frame, src.out_frame.max(src.in_frame));
        let source_sec = frame_to_seconds(source_frame, src.fps);
        let clip_id = src.clip_id.clone();
        let pid = session.project_id.clone();
        let paths = paths.clone();
        return tokio::task::spawn_blocking(move || {
            frame_from_clip(&paths, &pid, &clip_id, source_sec)
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    let active = resolve_active_layer_frame_public(session, virtual_frame);
    if active.video_blank && active.layer != ActiveLayerKind::Cover {
        return render_blank_frame().await;
    }
    let pid = session.project_id.clone();
    let paths = paths.clone();
    let layer = active.layer;
    let part_id = active.part_id;
    let cover_id = active.cover_id;
    let source_sec = active.source_sec;
    let local_frame = active.local_frame;
    let video_blank = active.video_blank;
    tokio::task::spawn_blocking(move || match layer {
        ActiveLayerKind::Cover if !cover_id.is_empty() => {
            frame_from_cover(&paths, &pid, &cover_id, source_sec)
        }
        ActiveLayerKind::Part if !part_id.is_empty() && !video_blank => {
            frame_from_part_at_frame(&paths, &pid, &part_id, local_frame)
        }
        _ => render_blank_frame_sync(),
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn render_blank_frame() -> Result<PathBuf, String> {
    tokio::task::spawn_blocking(render_blank_frame_sync)
        .await
        .map_err(|e| e.to_string())?
}

fn render_blank_frame_sync() -> Result<PathBuf, String> {
    let out = std::env::temp_dir()
        .join("qnc")
        .join("qstory_playback")
        .join(format!("blank_{}.jpg", uuid::Uuid::new_v4().simple()));
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let ffmpeg = resolve_ffmpeg().ok_or_else(|| "ffmpeg nije dostupan".to_string())?;
    let status = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=1920x1080:d=0.04",
            "-frames:v",
            "1",
            "-q:v",
            "3",
        ])
        .arg(&out)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() || !out.is_file() {
        return Err("preview blank frame nije generiran".into());
    }
    Ok(out)
}

fn frame_from_clip(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    source_sec: f64,
) -> Result<PathBuf, String> {
    let media = resolve_play_media(paths, project_id, clip_id)?;
    let out = cache_output_path("frame_clip", clip_id, source_sec);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    extract_preview_jpeg_at_seek(&media.path, &out, source_sec.max(0.0))?;
    Ok(out)
}

fn frame_from_part(
    paths: &ProjectPaths,
    project_id: &str,
    part_id: &str,
    local_sec: f64,
) -> Result<PathBuf, String> {
    let (clip_id, in_frame, out_frame, fps) = part_stream_frames(paths, project_id, part_id)?;
    let mux = block_on_cached(
        paths,
        project_id,
        &clip_id,
        in_frame,
        out_frame,
        fps,
        VirtualStreamKind::Mux,
    )?;
    let out = cache_output_path("frame", part_id, local_sec);
    extract_preview_jpeg_at_seek(&mux, &out, local_sec.max(0.0))?;
    Ok(out)
}

fn frame_from_part_at_frame(
    paths: &ProjectPaths,
    project_id: &str,
    part_id: &str,
    local_frame: i64,
) -> Result<PathBuf, String> {
    let (_, _, _, fps) = part_stream_frames(paths, project_id, part_id)?;
    frame_from_part(
        paths,
        project_id,
        part_id,
        frame_to_seconds(local_frame.max(0), fps),
    )
}

fn frame_from_cover(
    paths: &ProjectPaths,
    project_id: &str,
    cover_id: &str,
    source_sec: f64,
) -> Result<PathBuf, String> {
    let (clip_id, in_frame, out_frame, fps) = cover_stream_frames(paths, project_id, cover_id)?;
    let mux = block_on_cached(
        paths,
        project_id,
        &clip_id,
        in_frame,
        out_frame,
        fps,
        VirtualStreamKind::Mux,
    )?;
    let out = cache_output_path("frame", cover_id, source_sec);
    extract_preview_jpeg_at_seek(&mux, &out, source_sec.max(0.0))?;
    Ok(out)
}

fn block_on_cached(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    in_frame: i64,
    out_frame: i64,
    fps: f64,
    kind: VirtualStreamKind,
) -> Result<PathBuf, String> {
    let paths = paths.clone();
    let project_id = project_id.to_string();
    let clip_id = clip_id.to_string();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(ensure_virtual_stream_cached_kind(
                &paths,
                &project_id,
                &clip_id,
                in_frame,
                out_frame,
                fps,
                kind,
            ))
    })
    .join()
    .map_err(|_| "cache thread panic".to_string())?
}

fn cache_output_path(kind: &str, id: &str, sec: f64) -> PathBuf {
    std::env::temp_dir()
        .join("qnc")
        .join("qstory_playback")
        .join(format!(
            "{kind}_{}_{sec:.3}.jpg",
            id.replace(|c: char| !c.is_ascii_alphanumeric(), "_")
        ))
}

fn mix_cache_path(session_id: &str, from_sec: f64, dur: f64) -> PathBuf {
    std::env::temp_dir()
        .join("qnc")
        .join("qstory_playback")
        .join(format!(
            "mix_{}_{from_sec:.3}_{dur:.3}.m4a",
            session_id.replace(|c: char| !c.is_ascii_alphanumeric(), "_")
        ))
}

fn dual_mono_filter(part_ss: &str, cover_ss: &str, dur: &str) -> String {
    format!(
        "[0:a]atrim=start={part_ss}:duration={dur},asetpts=PTS-STARTPTS,\
         pan=mono|c0=c0,aresample=48000[a1];\
         [1:a]atrim=start={cover_ss}:duration={dur},asetpts=PTS-STARTPTS,\
         pan=mono|c0=c0,aresample=48000[a2];\
         [a1][a2]join=inputs=2:channel_layout=stereo:map=0.0-FL|1.0-FR[aout]"
    )
}

pub(crate) fn plan_mix_slices(
    session: &PlaybackSession,
    from_sec: f64,
    duration_sec: f64,
) -> Vec<MixSlice> {
    let fps = session.playlist.timeline_fps.max(1.0);
    let from_frame = seconds_to_frame(from_sec.max(0.0), fps);
    let duration_frames = seconds_to_frame(duration_sec.max(0.0), fps).max(1);
    let end_frame = from_frame.saturating_add(duration_frames);
    let mut slices = Vec::new();
    let mut frame = from_frame;
    while frame < end_frame {
        let (segment, local_frame) = find_segment_frame(&session.playlist, frame);
        let Some(segment) = segment else {
            break;
        };
        let segment_end_frame = segment.global_end_frame.max(segment.global_start_frame + 1);
        let cover = find_cover_frame(&segment.covers, frame);
        let mut boundary_frame = end_frame.min(segment_end_frame);
        if let Some(cover) = cover {
            boundary_frame = boundary_frame.min(cover.timeline_end_frame.max(frame + 1));
        } else {
            for c in &segment.covers {
                if c.streamable && c.timeline_start_frame > frame {
                    boundary_frame = boundary_frame.min(c.timeline_start_frame);
                }
            }
        }
        let dur_frames = (boundary_frame - frame).max(0);
        let dur = frame_to_seconds(dur_frames, fps).max(0.0);
        if dur <= EPS {
            break;
        }
        let part_local_in = frame_to_seconds(local_frame.max(0), fps);
        let (cover_id, cover_source_in) = if let Some(c) = cover {
            let source_fps = if is_valid_fps(c.source_fps) {
                c.source_fps
            } else {
                fps
            };
            let record_offset_frame = (frame - c.timeline_start_frame).max(0);
            let source_offset_frame =
                source_offset_for_record_frame(record_offset_frame, fps, source_fps);
            (
                Some(c.cover_id.clone()),
                frame_to_seconds(source_offset_frame, source_fps),
            )
        } else {
            (None, 0.0)
        };
        slices.push(MixSlice {
            part_id: segment.part_id.clone(),
            duration_sec: dur,
            part_local_in_sec: part_local_in,
            cover_id,
            cover_source_in_sec: cover_source_in,
        });
        frame = boundary_frame;
    }
    slices
}

fn render_source_audio_blocking(
    paths: &ProjectPaths,
    session_id: &str,
    project_id: &str,
    clip_id: &str,
    from_sec: f64,
    dur: f64,
) -> Result<PathBuf, String> {
    let out = mix_cache_path(session_id, from_sec, dur);
    if out.metadata().map(|m| m.len() > 512).unwrap_or(false) {
        return Ok(out);
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let media = resolve_play_media(paths, project_id, clip_id)?;
    let ffmpeg =
        resolve_ffmpeg().ok_or_else(|| "ffmpeg nije dostupan za playback mix".to_string())?;
    if media_has_audio_stream(&media.path) == Some(false) {
        render_silence_audio_file(&ffmpeg, &out, dur)?;
        return Ok(out);
    }
    let status = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-ss",
            &format!("{from_sec:.3}"),
            "-t",
            &format!("{dur:.3}"),
            "-i",
        ])
        .arg(&media.path)
        .args([
            "-vn",
            "-ac",
            "2",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
        ])
        .arg(&out)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() || !out.is_file() {
        return Err(format!(
            "playback audio: source clip render nije uspio ({clip_id})"
        ));
    }
    Ok(out)
}

fn render_mix_blocking(
    paths: &ProjectPaths,
    session_id: &str,
    project_id: &str,
    from_sec: f64,
    dur: f64,
    slices: &[MixSlice],
) -> Result<PathBuf, String> {
    let out = mix_cache_path(session_id, from_sec, dur);
    if out.metadata().map(|m| m.len() > 512).unwrap_or(false) {
        return Ok(out);
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let ffmpeg =
        resolve_ffmpeg().ok_or_else(|| "ffmpeg nije dostupan za playback mix".to_string())?;
    let work = out.with_extension("work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let mut slice_paths = Vec::new();
    for (index, slice) in slices.iter().enumerate() {
        let slice_path = work.join(format!("slice_{index:03}.m4a"));
        render_one_slice(paths, project_id, slice, &slice_path, &ffmpeg)?;
        slice_paths.push(slice_path);
    }
    if slice_paths.len() == 1 {
        std::fs::copy(&slice_paths[0], &out).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&work);
        return Ok(out);
    }
    let list_file = work.join("concat.txt");
    let list_body = slice_paths
        .iter()
        .map(|p| {
            let escaped = p.to_string_lossy().replace('\'', "'\\''");
            format!("file '{escaped}'")
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&list_file, list_body).map_err(|e| e.to_string())?;
    let status = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
        ])
        .arg(&list_file)
        .args(["-c", "copy"])
        .arg(&out)
        .status()
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&work);
    if !status.success() || !out.is_file() {
        return Err("ffmpeg concat program audio nije uspio".into());
    }
    Ok(out)
}

fn render_one_slice(
    paths: &ProjectPaths,
    project_id: &str,
    slice: &MixSlice,
    output: &Path,
    ffmpeg: &Path,
) -> Result<(), String> {
    let (clip_id, in_frame, out_frame, fps) =
        part_stream_frames(paths, project_id, &slice.part_id)?;
    let part_audio = block_on_cached(
        paths,
        project_id,
        &clip_id,
        in_frame,
        out_frame,
        fps,
        VirtualStreamKind::AudioOnly,
    )?;
    let dur = format!("{:.6}", slice.duration_sec.max(EPS));
    let part_ss = format!("{:.6}", slice.part_local_in_sec.max(0.0));

    if let Some(cover_id) = slice.cover_id.as_ref() {
        let (c_clip, c_in, c_out, c_fps) = cover_stream_frames(paths, project_id, cover_id)?;
        let cover_audio = block_on_cached(
            paths,
            project_id,
            &c_clip,
            c_in,
            c_out,
            c_fps,
            VirtualStreamKind::AudioOnly,
        )?;
        let cover_ss = format!("{:.6}", slice.cover_source_in_sec.max(0.0));
        let filter = dual_mono_filter(&part_ss, &cover_ss, &dur);
        let status = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .arg("-i")
            .arg(&part_audio)
            .arg("-i")
            .arg(&cover_audio)
            .args(["-filter_complex", &filter, "-map", "[aout]"])
            .args(["-c:a", "aac", "-b:a", "192k", "-ar", "48000"])
            .arg(output)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("dual-mono slice failed for cover {cover_id}"));
        }
        return Ok(());
    }

    let filter = dual_mono_filter(&part_ss, "0", &dur);
    let status = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(&part_audio)
        .args([
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=mono:sample_rate=48000",
            "-filter_complex",
            &filter,
            "-map",
            "[aout]",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ar",
            "48000",
        ])
        .arg(output)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!(
            "dual-mono A1 slice failed for part {}",
            slice.part_id
        ));
    }
    Ok(())
}

fn render_silence_audio_file(
    ffmpeg: &Path,
    output: &Path,
    duration_sec: f64,
) -> Result<(), String> {
    let duration = duration_sec.max(0.001);
    let status = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=stereo:sample_rate=48000",
            "-t",
            &format!("{duration:.9}"),
            "-vn",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ar",
            "48000",
            "-movflags",
            "+faststart",
        ])
        .arg(output)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() || !output.is_file() {
        return Err("playback audio: silence lane nije generiran".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::dual_mono_filter;

    #[test]
    fn dual_mono_filter_keeps_a1_and_a2_separate() {
        let filter = dual_mono_filter("1.000000", "2.000000", "3.000000");

        assert!(filter.contains("pan=mono|c0=c0"));
        assert!(filter.contains("join=inputs=2:channel_layout=stereo"));
        assert!(filter.contains("map=0.0-FL|1.0-FR"));
        assert!(!filter.contains("amix"));
    }
}
