//! F2 — server-side mixed audio (A1 + A2 u cover slotu) i preview frame.
//! Play media path: proxy-only via `media::resolve_play_media` (see docs/qnc-playback-engine.md).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::editor_assets::{ensure_virtual_stream_cached_kind, VirtualStreamKind};
use crate::ingest::thumb::{extract_preview_jpeg_at_seek, media_has_audio_stream, resolve_ffmpeg};
use crate::media::resolve_play_media;
use crate::project::db::ProjectPaths;

use super::db::{cover_stream_frames, part_stream_frames};
use super::playback::{
    find_cover, find_segment, resolve_active_layer_public, ActiveLayerKind, PlaybackSession,
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

pub async fn render_preview_frame(
    paths: &ProjectPaths,
    session: &PlaybackSession,
    virtual_sec: f64,
) -> Result<PathBuf, String> {
    if let Some(src) = session.source_clip.as_ref() {
        let source_sec = virtual_sec.max(src.in_sec).min(src.out_sec.max(src.in_sec));
        let clip_id = src.clip_id.clone();
        let pid = session.project_id.clone();
        let paths = paths.clone();
        return tokio::task::spawn_blocking(move || {
            frame_from_clip(&paths, &pid, &clip_id, source_sec)
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    let active = resolve_active_layer_public(session, virtual_sec);
    if active.video_blank && active.layer != ActiveLayerKind::Cover {
        return render_blank_frame().await;
    }
    let pid = session.project_id.clone();
    let paths = paths.clone();
    let layer = active.layer;
    let part_id = active.part_id;
    let cover_id = active.cover_id;
    let source_sec = active.source_sec;
    let local_sec = active.local_sec;
    let video_blank = active.video_blank;
    tokio::task::spawn_blocking(move || match layer {
        ActiveLayerKind::Cover if !cover_id.is_empty() => {
            frame_from_cover(&paths, &pid, &cover_id, source_sec)
        }
        ActiveLayerKind::Part if !part_id.is_empty() && !video_blank => {
            frame_from_part(&paths, &pid, &part_id, local_sec)
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

pub(crate) fn plan_mix_slices(
    session: &PlaybackSession,
    from_sec: f64,
    duration_sec: f64,
) -> Vec<MixSlice> {
    let end = from_sec + duration_sec;
    let mut slices = Vec::new();
    let mut t = from_sec;
    while t < end - EPS {
        let Some(segment) = find_segment(&session.playlist, t).0 else {
            break;
        };
        let slice_end = end.min(segment.global_end_sec);
        let cover = find_cover(&segment.covers, t);
        let mut boundary = slice_end;
        if let Some(cover) = cover {
            boundary = boundary.min(cover.timeline_end_sec);
        } else {
            for c in &segment.covers {
                if c.streamable && c.timeline_start_sec > t + EPS {
                    boundary = boundary.min(c.timeline_start_sec);
                }
            }
        }
        let dur = (boundary - t).max(0.0);
        if dur <= EPS {
            break;
        }
        let part_local_in = (t - segment.global_start_sec).max(0.0);
        let (cover_id, cover_source_in) = if let Some(c) = cover {
            (
                Some(c.cover_id.clone()),
                (t - c.timeline_start_sec).max(0.0) + c.source_offset_sec.max(0.0),
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
        t = boundary;
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
            "playback audio: source clip mix nije uspio ({clip_id})"
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
        return Err("ffmpeg concat mix nije uspio".into());
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
        let filter = format!(
            "[0:a]atrim=start={part_ss}:duration={dur},asetpts=PTS-STARTPTS[a1];\
             [1:a]atrim=start={cover_ss}:duration={dur},asetpts=PTS-STARTPTS[a2];\
             [a1][a2]amix=inputs=2:duration=longest:dropout_transition=0[aout]"
        );
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
            return Err(format!("amix slice failed for cover {cover_id}"));
        }
        return Ok(());
    }

    let status = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-ss")
        .arg(&part_ss)
        .arg("-i")
        .arg(&part_audio)
        .arg("-t")
        .arg(&dur)
        .args(["-vn", "-c:a", "aac", "-b:a", "192k", "-ar", "48000"])
        .arg(output)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("a1 slice failed for part {}", slice.part_id));
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
