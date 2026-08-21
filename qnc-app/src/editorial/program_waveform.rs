//! Passive program waveform projection.
//!
//! This module never decodes media and never controls playback. It maps existing
//! per-source waveform peaks from the asset cache onto the editorial program
//! axis using source IN/OUT frame ranges.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::api::HostClient;
use crate::editorial::segment_program::{SegmentProgramCover, SegmentProgramModel};
use crate::editorial::types::StoryShot;
use crate::media_assets::{AsyncWaveformAssetLoader, WaveformAsset};

const PROGRAM_PEAK_BUCKETS: usize = 1200;
const MIN_PROGRAM_PEAK_BUCKETS: usize = 24;
const WAVEFORM_RETRY_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default)]
pub(crate) struct ProgramWaveform {
    pub a1_peaks: Vec<f32>,
    pub a2_peaks: Vec<f32>,
}

#[derive(Default)]
pub(crate) struct ProgramWaveformAssets {
    loader: AsyncWaveformAssetLoader,
    cache: HashMap<String, WaveformAsset>,
    retry_after: HashMap<String, Instant>,
}

impl ProgramWaveformAssets {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn poll(&mut self, project_id: &str, ctx: &egui::Context) {
        let now = Instant::now();
        for result in self.loader.poll() {
            if result.project_id != project_id {
                continue;
            }
            match result.waveform {
                Ok(waveform) if !waveform.a1_peaks.is_empty() || !waveform.a2_peaks.is_empty() => {
                    self.retry_after.remove(&result.clip_id);
                    self.cache.insert(result.clip_id, waveform);
                    ctx.request_repaint();
                }
                Ok(_) | Err(_) => {
                    self.retry_after
                        .insert(result.clip_id, now + WAVEFORM_RETRY_DELAY);
                    ctx.request_repaint_after(WAVEFORM_RETRY_DELAY);
                }
            }
        }
    }

    pub fn request_for_program(
        &mut self,
        host: &HostClient,
        project_id: &str,
        program: &SegmentProgramModel,
        ctx: &egui::Context,
    ) {
        let now = Instant::now();
        for clip_id in program_clip_ids(program) {
            if self.cache.contains_key(clip_id.as_str()) {
                continue;
            }
            if self
                .retry_after
                .get(clip_id.as_str())
                .is_some_and(|retry| *retry > now)
            {
                continue;
            }
            let _ = self
                .loader
                .request(host, project_id.to_string(), clip_id, Some(ctx.clone()));
        }
    }

    pub fn compose(
        &self,
        program: &SegmentProgramModel,
        source_duration_frames: &HashMap<String, i64>,
    ) -> ProgramWaveform {
        compose_program_waveform(program, &self.cache, source_duration_frames)
    }
}

pub(crate) fn source_duration_frames(
    all_clips: &[StoryShot],
    virtual_shots: &[StoryShot],
) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for clip in all_clips {
        remember_source_duration(&mut out, clip);
    }
    for clip in virtual_shots {
        let clip_id = clip.clip_id.trim();
        if clip_id.is_empty() {
            continue;
        }
        let duration = clip.duration_frames.max(clip.out_frame).max(0);
        if duration <= 0 {
            continue;
        }
        out.entry(clip_id.to_string()).or_insert(duration);
    }
    out
}

fn remember_source_duration(out: &mut HashMap<String, i64>, clip: &StoryShot) {
    let clip_id = clip.clip_id.trim();
    if clip_id.is_empty() {
        return;
    }
    let duration = clip.duration_frames.max(clip.out_frame).max(0);
    if duration <= 0 {
        return;
    }
    out.entry(clip_id.to_string())
        .and_modify(|stored| *stored = (*stored).max(duration))
        .or_insert(duration);
}

fn program_clip_ids(program: &SegmentProgramModel) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for segment in program.segments() {
        let clip_id = segment.clip_id.trim();
        if segment.streamable && !clip_id.is_empty() && seen.insert(clip_id.to_string()) {
            out.push(clip_id.to_string());
        }
    }
    for cover in program.covers() {
        let clip_id = cover.clip_id.trim();
        if cover.streamable && !clip_id.is_empty() && seen.insert(clip_id.to_string()) {
            out.push(clip_id.to_string());
        }
    }
    out
}

fn compose_program_waveform(
    program: &SegmentProgramModel,
    waveforms: &HashMap<String, WaveformAsset>,
    source_duration_frames: &HashMap<String, i64>,
) -> ProgramWaveform {
    if program.is_empty() {
        return ProgramWaveform::default();
    }
    let program_duration = program.duration_frames().max(1);
    let buckets = (program_duration as usize).clamp(MIN_PROGRAM_PEAK_BUCKETS, PROGRAM_PEAK_BUCKETS);
    let mut out = ProgramWaveform {
        a1_peaks: vec![0.0; buckets],
        a2_peaks: vec![0.0; buckets],
    };

    for segment in program.segments() {
        let clip_id = segment.clip_id.trim();
        let Some(waveform) = waveforms.get(clip_id) else {
            continue;
        };
        let Some(source_duration) = source_duration_frames.get(clip_id).copied() else {
            continue;
        };
        let segment_peaks = primary_source_peaks(waveform);
        fill_program_peaks(
            &mut out.a1_peaks,
            program_duration,
            segment.global_start_frame,
            segment.global_end_frame,
            segment_peaks,
            source_duration,
            segment.source_in_frame,
            segment.source_out_frame,
        );
    }

    for cover in program.covers() {
        let clip_id = cover.clip_id.trim();
        let Some(waveform) = waveforms.get(clip_id) else {
            continue;
        };
        let Some(source_duration) = source_duration_frames.get(clip_id).copied() else {
            continue;
        };
        let cover_peaks = primary_source_peaks(waveform);
        fill_cover_peaks(
            &mut out.a2_peaks,
            program_duration,
            cover,
            cover_peaks,
            source_duration,
        );
    }

    out
}

fn primary_source_peaks(waveform: &WaveformAsset) -> &[f32] {
    if waveform.a1_peaks.is_empty() {
        &waveform.a2_peaks
    } else {
        &waveform.a1_peaks
    }
}

fn fill_cover_peaks(
    out: &mut [f32],
    program_duration: i64,
    cover: &SegmentProgramCover,
    source_peaks: &[f32],
    source_duration: i64,
) {
    fill_program_peaks(
        out,
        program_duration,
        cover.start_frame,
        cover.end_frame,
        source_peaks,
        source_duration,
        cover.source_in_frame,
        cover.source_out_frame,
    );
}

fn fill_program_peaks(
    out: &mut [f32],
    program_duration: i64,
    program_start_frame: i64,
    program_end_frame: i64,
    source_peaks: &[f32],
    source_duration_frames: i64,
    source_in_frame: i64,
    source_out_frame: i64,
) {
    if out.is_empty() || source_peaks.is_empty() {
        return;
    }
    let program_start = program_start_frame.max(0);
    let program_end = program_end_frame.max(program_start + 1);
    let source_duration = source_duration_frames.max(1);
    let source_in = source_in_frame.clamp(0, source_duration);
    let source_out = source_out_frame.max(source_in + 1).min(source_duration);
    if source_out <= source_in {
        return;
    }

    let target_start = bucket_floor(program_start, program_duration, out.len());
    let target_end = bucket_ceil(program_end, program_duration, out.len()).min(out.len());
    for bucket in target_start..target_end {
        let bucket_program_start =
            bucket as f64 * program_duration as f64 / out.len().max(1) as f64;
        let bucket_program_end =
            (bucket + 1) as f64 * program_duration as f64 / out.len().max(1) as f64;
        let intersect_start = bucket_program_start.max(program_start as f64);
        let intersect_end = bucket_program_end.min(program_end as f64);
        if intersect_end <= intersect_start {
            continue;
        }
        let source_start = map_program_to_source(
            intersect_start,
            program_start as f64,
            program_end as f64,
            source_in as f64,
            source_out as f64,
        );
        let source_end = map_program_to_source(
            intersect_end,
            program_start as f64,
            program_end as f64,
            source_in as f64,
            source_out as f64,
        );
        out[bucket] = out[bucket].max(max_source_peak(
            source_peaks,
            source_duration,
            source_start,
            source_end,
        ));
    }
}

fn bucket_floor(frame: i64, duration_frames: i64, buckets: usize) -> usize {
    ((frame.max(0) as f64 / duration_frames.max(1) as f64) * buckets.max(1) as f64)
        .floor()
        .clamp(0.0, buckets as f64) as usize
}

fn bucket_ceil(frame: i64, duration_frames: i64, buckets: usize) -> usize {
    ((frame.max(0) as f64 / duration_frames.max(1) as f64) * buckets.max(1) as f64)
        .ceil()
        .clamp(0.0, buckets as f64) as usize
}

fn map_program_to_source(
    program_frame: f64,
    program_start: f64,
    program_end: f64,
    source_in: f64,
    source_out: f64,
) -> f64 {
    let program_span = (program_end - program_start).max(1.0);
    let t = ((program_frame - program_start) / program_span).clamp(0.0, 1.0);
    source_in + t * (source_out - source_in).max(1.0)
}

fn max_source_peak(
    source_peaks: &[f32],
    source_duration_frames: i64,
    source_start_frame: f64,
    source_end_frame: f64,
) -> f32 {
    if source_peaks.is_empty() {
        return 0.0;
    }
    let duration = source_duration_frames.max(1) as f64;
    let peak_len = source_peaks.len();
    let start = ((source_start_frame.max(0.0) / duration) * peak_len as f64)
        .floor()
        .clamp(0.0, peak_len as f64) as usize;
    let end = ((source_end_frame.max(source_start_frame + 1.0) / duration) * peak_len as f64)
        .ceil()
        .clamp(0.0, peak_len as f64) as usize;
    let end = end.max(start + 1).min(peak_len);
    source_peaks[start..end].iter().copied().fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EditorialPlaylist, EditorialPlaylistCover, EditorialPlaylistSegment};

    fn waveform(a1_peaks: Vec<f32>, a2_peaks: Vec<f32>) -> WaveformAsset {
        WaveformAsset { a1_peaks, a2_peaks }
    }

    #[test]
    fn base_segments_map_source_peaks_to_program_a1() {
        let playlist = EditorialPlaylist {
            duration_frames: 100,
            segments: vec![EditorialPlaylistSegment {
                part_id: "part_a".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 0,
                global_end_frame: 100,
                duration_frames: 100,
                source_in_frame: 100,
                source_out_frame: 200,
                streamable: true,
                ..EditorialPlaylistSegment::default()
            }],
            ..EditorialPlaylist::default()
        };
        let program = SegmentProgramModel::from_playlist(Some(&playlist), &[], &[], &[]);
        let waveforms =
            HashMap::from([("clip_a".into(), waveform(vec![0.1, 0.8, 0.2, 0.3], vec![]))]);
        let durations = HashMap::from([("clip_a".into(), 400)]);

        let composed = compose_program_waveform(&program, &waveforms, &durations);

        assert!(composed.a1_peaks.iter().any(|peak| *peak >= 0.8));
        assert!(composed.a2_peaks.iter().all(|peak| *peak == 0.0));
    }

    #[test]
    fn covers_map_primary_source_audio_to_program_a2() {
        let playlist = EditorialPlaylist {
            duration_frames: 100,
            segments: vec![EditorialPlaylistSegment {
                part_id: "part_a".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 0,
                global_end_frame: 100,
                duration_frames: 100,
                source_in_frame: 0,
                source_out_frame: 100,
                streamable: true,
                covers: vec![EditorialPlaylistCover {
                    cover_id: "cover_a".into(),
                    clip_id: "clip_b".into(),
                    timeline_start_frame: 25,
                    timeline_end_frame: 75,
                    source_in_frame: 200,
                    source_out_frame: 300,
                    streamable: true,
                    ..EditorialPlaylistCover::default()
                }],
                ..EditorialPlaylistSegment::default()
            }],
            ..EditorialPlaylist::default()
        };
        let program = SegmentProgramModel::from_playlist(Some(&playlist), &[], &[], &[]);
        let waveforms = HashMap::from([
            ("clip_a".into(), waveform(vec![0.1], vec![])),
            ("clip_b".into(), waveform(vec![0.1, 0.2, 0.9, 0.3], vec![])),
        ]);
        let durations = HashMap::from([("clip_a".into(), 100), ("clip_b".into(), 400)]);

        let composed = compose_program_waveform(&program, &waveforms, &durations);

        assert!(composed.a2_peaks.iter().any(|peak| *peak >= 0.9));
    }

    #[test]
    fn base_segment_uses_a2_when_a1_is_missing() {
        let playlist = EditorialPlaylist {
            duration_frames: 50,
            segments: vec![EditorialPlaylistSegment {
                part_id: "part_a".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 0,
                global_end_frame: 50,
                duration_frames: 50,
                source_in_frame: 0,
                source_out_frame: 50,
                streamable: true,
                ..EditorialPlaylistSegment::default()
            }],
            ..EditorialPlaylist::default()
        };
        let program = SegmentProgramModel::from_playlist(Some(&playlist), &[], &[], &[]);
        let waveforms = HashMap::from([("clip_a".into(), waveform(vec![], vec![0.0, 0.6, 0.0]))]);
        let durations = HashMap::from([("clip_a".into(), 50)]);

        let composed = compose_program_waveform(&program, &waveforms, &durations);

        assert!(composed.a1_peaks.iter().any(|peak| *peak >= 0.6));
    }
}
