//! Playlist-input playback request builder.
//!
//! This component owns EDL construction for playlist-input playout. UI forms
//! provide snapshots; the Broadcast Player owns transport once the playlist
//! input is opened.

use crate::editorial::segment_program::{
    SegmentProgramCover, SegmentProgramModel, SegmentProgramSegment,
};
use crate::editorial::types::{StoryCover, StoryShot};
use crate::frame_time::normalize_fps;
use crate::player_contract::{BroadcastHostSourceRef, FrameNumber};
use crate::player_remote::{
    BroadcastProgramItem, BroadcastProgramOpenRequest, BroadcastProgramSource,
    PROGRAM_AUDIO_OUTPUT_CH1, PROGRAM_AUDIO_OUTPUT_CH2,
};

pub(crate) struct EditorialProgramPlaybackComponent;

pub(crate) struct EditorialProgramPlaybackInput<'a> {
    pub project_id: &'a str,
    pub program_id: &'a str,
    pub start_program_frame: i64,
    pub program: &'a SegmentProgramModel,
    pub covers: &'a [StoryCover],
    pub clips: &'a [StoryShot],
}

impl EditorialProgramPlaybackComponent {
    pub(crate) fn build_program(
        input: EditorialProgramPlaybackInput<'_>,
    ) -> Result<BroadcastProgramOpenRequest, String> {
        let project_id = input.project_id.trim();
        if project_id.is_empty() {
            return Err("Nema otvorenog projekta".into());
        }
        let timeline_fps = input
            .program
            .timeline_fps()
            .ok_or_else(|| "Playlist input nema valjan timeline FPS".to_string())?;
        let duration_frames = input.program.duration_frames().max(0);
        if duration_frames <= 0 || input.program.is_empty() {
            return Err("Playlist input je prazan".into());
        }
        let _ = input.covers;

        let mut items = Vec::new();
        for program_range in input.program.segments() {
            for item in
                ItemBuilder::from_program_range(program_range, input.program.covers(), input.clips)?
            {
                items.push(item.finish(project_id)?);
            }
        }
        let items = collapse_contiguous_single_source_items(items);
        if items.is_empty() {
            return Err("Playlist input nema streamable item".into());
        }

        Ok(BroadcastProgramOpenRequest {
            program_id: non_empty_or(input.program_id, "playlist-input"),
            project_id: project_id.to_string(),
            timeline_fps,
            duration_frames,
            start_program_frame: FrameNumber(
                input
                    .start_program_frame
                    .max(0)
                    .min(duration_frames.saturating_sub(1)),
            ),
            items,
        })
    }
}

#[derive(Debug, Clone)]
struct ItemBuilder {
    item_id: String,
    record_in_frame: i64,
    record_out_frame: i64,
    sources: Vec<MediaBuilder>,
}

#[derive(Debug, Clone)]
struct MediaBuilder {
    shot_id: String,
    clip_id: String,
    virtual_shot_id: String,
    source_in_frame: i64,
    source_out_frame: i64,
    source_fps: f64,
    source_duration_frames: i64,
    media_input: String,
    has_video: bool,
    has_audio: bool,
    audio_channels: u8,
    audio_output_channel: Option<u8>,
}

impl ItemBuilder {
    fn from_program_range(
        segment: &SegmentProgramSegment,
        covers: &[SegmentProgramCover],
        clips: &[StoryShot],
    ) -> Result<Vec<Self>, String> {
        let clip_id = segment.clip_id.trim();
        if clip_id.is_empty() {
            return Err("EDL red nema clip_id".into());
        }
        let clip = clip_by_id(clips, clip_id)?;
        let source_fps = resolve_source_fps(clip, segment.source_fps)?;
        let is_off = segment.kind.trim().eq_ignore_ascii_case("offovi");
        let segment_start = segment.global_start_frame.max(0);
        let segment_end = segment.global_end_frame.max(segment_start + 1);
        let source_in_frame = segment.source_in_frame.max(0);
        let source_out_frame = segment.source_out_frame.max(source_in_frame + 1);
        let source_duration_frames = clip.duration_frames.max(source_out_frame).max(1);
        let media_input = clip.play_path.trim().to_string();
        let has_base_audio = if is_off { true } else { clip.has_audio };
        let mut items = Vec::new();

        let cover_ranges = cover_ranges_for_segment(covers, segment_start, segment_end);
        let mut cursor = segment_start;
        for cover in cover_ranges {
            let cover_start = cover.start_frame.max(segment_start).max(cursor);
            let cover_end = cover.end_frame.min(segment_end).max(cover_start);
            if cover_end <= cover_start {
                continue;
            }
            if cursor < cover_start {
                items.push(Self::from_base_item(
                    segment,
                    clip,
                    source_fps,
                    source_duration_frames,
                    &media_input,
                    has_base_audio,
                    is_off,
                    cursor,
                    cover_start,
                ));
            }

            let mut sources = Vec::new();
            if has_base_audio {
                sources.push(MediaBuilder::from_segment_chunk(
                    segment,
                    clip,
                    source_fps,
                    source_duration_frames,
                    &media_input,
                    false,
                    true,
                    Some(PROGRAM_AUDIO_OUTPUT_CH1),
                    cover_start,
                    cover_end,
                ));
            }
            sources.push(MediaBuilder::from_cover_chunk(
                cover,
                clips,
                true,
                Some(PROGRAM_AUDIO_OUTPUT_CH2),
                cover_start,
                cover_end,
            )?);

            items.push(Self {
                item_id: item_id_for_range(cover_start, cover_end),
                record_in_frame: cover_start,
                record_out_frame: cover_end,
                sources,
            });
            cursor = cover_end;
        }

        if cursor < segment_end {
            items.push(Self::from_base_item(
                segment,
                clip,
                source_fps,
                source_duration_frames,
                &media_input,
                has_base_audio,
                is_off,
                cursor,
                segment_end,
            ));
        }

        Ok(items)
    }

    fn from_base_item(
        segment: &SegmentProgramSegment,
        clip: &StoryShot,
        source_fps: f64,
        source_duration_frames: i64,
        media_input: &str,
        has_base_audio: bool,
        is_off: bool,
        record_in_frame: i64,
        record_out_frame: i64,
    ) -> Self {
        let mut sources = Vec::new();
        if !is_off {
            sources.push(MediaBuilder::from_segment_chunk(
                segment,
                clip,
                source_fps,
                source_duration_frames,
                media_input,
                true,
                false,
                None,
                record_in_frame,
                record_out_frame,
            ));
        }
        if has_base_audio {
            sources.push(MediaBuilder::from_segment_chunk(
                segment,
                clip,
                source_fps,
                source_duration_frames,
                media_input,
                false,
                true,
                Some(PROGRAM_AUDIO_OUTPUT_CH1),
                record_in_frame,
                record_out_frame,
            ));
        }
        Self {
            item_id: item_id_for_range(record_in_frame, record_out_frame),
            record_in_frame,
            record_out_frame,
            sources,
        }
    }

    fn finish(self, project_id: &str) -> Result<BroadcastProgramItem, String> {
        Ok(BroadcastProgramItem {
            item_id: self.item_id,
            record_in_frame: FrameNumber(self.record_in_frame.max(0)),
            record_out_frame: FrameNumber(self.record_out_frame.max(self.record_in_frame + 1)),
            sources: self
                .sources
                .into_iter()
                .map(|media| media.finish(project_id))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

impl MediaBuilder {
    fn from_segment_chunk(
        segment: &SegmentProgramSegment,
        clip: &StoryShot,
        source_fps: f64,
        source_duration_frames: i64,
        media_input: &str,
        has_video: bool,
        has_audio: bool,
        audio_output_channel: Option<u8>,
        record_in_frame: i64,
        record_out_frame: i64,
    ) -> Self {
        let (source_in_frame, source_out_frame) = source_range_for_record_chunk(
            segment.source_in_frame,
            segment.source_out_frame,
            segment.global_start_frame,
            record_in_frame,
            record_out_frame,
        );
        Self {
            shot_id: non_empty_or(&segment.virtual_shot_id, &segment.clip_id),
            clip_id: segment.clip_id.trim().to_string(),
            virtual_shot_id: segment.virtual_shot_id.clone(),
            source_in_frame,
            source_out_frame,
            source_fps,
            source_duration_frames,
            media_input: media_input.to_string(),
            has_video,
            has_audio,
            audio_channels: audio_channels_for(has_audio, clip),
            audio_output_channel,
        }
    }

    fn from_cover_chunk(
        cover: &SegmentProgramCover,
        clips: &[StoryShot],
        has_video: bool,
        audio_output_channel: Option<u8>,
        record_in_frame: i64,
        record_out_frame: i64,
    ) -> Result<Self, String> {
        let clip_id = cover.clip_id.trim();
        if clip_id.is_empty() {
            return Err("EDL cover nema clip_id".into());
        }
        let clip = clip_by_id(clips, clip_id)?;
        let source_fps = resolve_source_fps(clip, cover.source_fps)?;
        let (source_in_frame, source_out_frame) = source_range_for_record_chunk(
            cover.source_in_frame,
            cover.source_out_frame,
            cover.start_frame,
            record_in_frame,
            record_out_frame,
        );
        Ok(Self {
            shot_id: non_empty_or(&cover.virtual_shot_id, clip_id),
            clip_id: clip_id.to_string(),
            virtual_shot_id: cover.virtual_shot_id.clone(),
            source_in_frame,
            source_out_frame,
            source_fps,
            source_duration_frames: clip.duration_frames.max(source_out_frame).max(1),
            media_input: clip.play_path.trim().to_string(),
            has_video,
            has_audio: clip.has_audio,
            audio_channels: audio_channels_for(clip.has_audio, clip),
            audio_output_channel: clip.has_audio.then_some(audio_output_channel).flatten(),
        })
    }

    fn finish(self, project_id: &str) -> Result<BroadcastProgramSource, String> {
        if self.media_input.trim().is_empty() {
            return Err(format!("Proxy path prazan · {}", self.clip_id));
        }
        let source_in = self.source_in_frame.max(0);
        let source_out = self.source_out_frame.max(source_in + 1);
        let source_ref = BroadcastHostSourceRef::from_frame_fields(
            project_id,
            self.shot_id,
            self.virtual_shot_id,
            &self.clip_id,
            Some(FrameNumber(source_in)),
            Some(FrameNumber(source_out)),
            FrameNumber(self.source_duration_frames.max(source_out).max(1)),
        )
        .map_err(|err| err.message)?;
        Ok(BroadcastProgramSource {
            source_ref,
            media_input: self.media_input,
            source_fps: self.source_fps,
            has_video: self.has_video,
            has_audio: self.has_audio,
            audio_channels: self.audio_channels,
            audio_output_channel: self.audio_output_channel,
        })
    }
}

fn cover_ranges_for_segment<'a>(
    covers: &'a [SegmentProgramCover],
    segment_start: i64,
    segment_end: i64,
) -> Vec<&'a SegmentProgramCover> {
    let mut ranges = covers
        .iter()
        .filter(|cover| {
            cover.streamable
                && cover.end_frame > segment_start
                && cover.start_frame < segment_end
                && cover.end_frame > cover.start_frame
        })
        .collect::<Vec<_>>();
    ranges.sort_by_key(|cover| (cover.start_frame, cover.end_frame, cover.cover_id.clone()));
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

fn clip_by_id<'a>(clips: &'a [StoryShot], clip_id: &str) -> Result<&'a StoryShot, String> {
    clips
        .iter()
        .find(|clip| clip.clip_id.trim() == clip_id)
        .ok_or_else(|| format!("Clip nije u snapshotu · {clip_id}"))
}

fn resolve_source_fps(clip: &StoryShot, source_fps: f64) -> Result<f64, String> {
    (source_fps.is_finite() && source_fps > 0.0)
        .then_some(source_fps)
        .or_else(|| (clip.fps.is_finite() && clip.fps > 0.0).then_some(clip.fps))
        .map(normalize_fps)
        .ok_or_else(|| format!("Clip '{}' nema valjan FPS", clip.clip_id))
}

fn audio_channels_for(has_audio: bool, clip: &StoryShot) -> u8 {
    if has_audio {
        clip.audio_channels.max(1)
    } else {
        0
    }
}

fn item_id_for_range(record_in_frame: i64, record_out_frame: i64) -> String {
    format!(
        "item:{}-{}",
        record_in_frame.max(0),
        record_out_frame.max(record_in_frame + 1)
    )
}

fn collapse_contiguous_single_source_items(
    items: Vec<BroadcastProgramItem>,
) -> Vec<BroadcastProgramItem> {
    let mut collapsed: Vec<BroadcastProgramItem> = Vec::new();
    for item in items {
        if let Some(previous) = collapsed.last_mut() {
            if merge_contiguous_item(previous, &item) {
                continue;
            }
        }
        collapsed.push(item);
    }
    collapsed
}

fn merge_contiguous_item(previous: &mut BroadcastProgramItem, next: &BroadcastProgramItem) -> bool {
    if previous.sources.is_empty() || previous.sources.len() != next.sources.len() {
        return false;
    }
    if previous.record_out_frame != next.record_in_frame {
        return false;
    }
    for (previous_source, next_source) in previous.sources.iter().zip(next.sources.iter()) {
        if !can_merge_contiguous_source(previous_source, next_source) {
            return false;
        }
    }

    previous.record_out_frame = next.record_out_frame;
    previous.item_id = item_id_for_range(previous.record_in_frame.0, previous.record_out_frame.0);
    for (previous_source, next_source) in previous.sources.iter_mut().zip(next.sources.iter()) {
        merge_source_out(previous_source, next_source);
    }
    true
}

fn can_merge_contiguous_source(
    previous: &BroadcastProgramSource,
    next: &BroadcastProgramSource,
) -> bool {
    if !same_program_source_track(previous, next) {
        return false;
    }
    let Some(previous_source_out) = previous.source_ref.out_frame else {
        return false;
    };
    let Some(next_source_in) = next.source_ref.in_frame else {
        return false;
    };
    previous_source_out == next_source_in && next.source_ref.out_frame.is_some()
}

fn merge_source_out(previous: &mut BroadcastProgramSource, next: &BroadcastProgramSource) {
    let Some(next_source_out) = next.source_ref.out_frame else {
        return;
    };
    previous.source_ref.out_frame = Some(next_source_out);
    previous.source_ref.duration_frames = FrameNumber(
        previous
            .source_ref
            .duration_frames
            .0
            .max(next.source_ref.duration_frames.0)
            .max(next_source_out.0),
    );
}

fn same_program_source_track(
    left: &BroadcastProgramSource,
    right: &BroadcastProgramSource,
) -> bool {
    left.source_ref.project_id == right.source_ref.project_id
        && left.source_ref.clip_id == right.source_ref.clip_id
        && media_input_key(&left.media_input) == media_input_key(&right.media_input)
        && approx_fps(left.source_fps, right.source_fps)
        && left.has_video == right.has_video
        && left.has_audio == right.has_audio
        && left.audio_channels == right.audio_channels
        && left.audio_output_channel == right.audio_output_channel
}

fn media_input_key(value: &str) -> String {
    value
        .trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn approx_fps(left: f64, right: f64) -> bool {
    (normalize_fps(left) - normalize_fps(right)).abs() < 0.01
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EditorialPlaylist, EditorialPlaylistCover, EditorialPlaylistSegment};

    fn clip(id: &str) -> StoryShot {
        StoryShot {
            clip_id: id.into(),
            fps: 50.0,
            duration_frames: 300,
            play_path: format!("C:/qnc/proxy/{id}.mp4"),
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        }
    }

    fn program(segments: Vec<EditorialPlaylistSegment>) -> SegmentProgramModel {
        SegmentProgramModel::from_playlist(
            Some(&EditorialPlaylist {
                project_id: "p".into(),
                timeline_fps: 50.0,
                duration_frames: 100,
                duration_sec: 2.0,
                segments,
            }),
            &[],
            &[],
            &[],
        )
    }

    fn video_source(item: &BroadcastProgramItem) -> &BroadcastProgramSource {
        item.sources
            .iter()
            .rev()
            .find(|source| source.has_video)
            .expect("video source")
    }

    fn audio_source(item: &BroadcastProgramItem, output_channel: u8) -> &BroadcastProgramSource {
        item.sources
            .iter()
            .find(|source| source.has_audio && source.audio_output_channel == Some(output_channel))
            .expect("audio source")
    }

    #[test]
    fn builds_program_request_with_playlist_items() {
        let program = program(vec![
            EditorialPlaylistSegment {
                part_id: "part_a".into(),
                kind: "tonovi".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 0,
                global_end_frame: 50,
                duration_frames: 50,
                source_in_frame: 100,
                source_out_frame: 150,
                source_fps: 59.94,
                streamable: true,
                ..EditorialPlaylistSegment::default()
            },
            EditorialPlaylistSegment {
                part_id: "part_b".into(),
                kind: "tonovi".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 50,
                global_end_frame: 100,
                duration_frames: 50,
                source_in_frame: 200,
                source_out_frame: 250,
                source_fps: 59.94,
                streamable: true,
                ..EditorialPlaylistSegment::default()
            },
        ]);

        let request =
            EditorialProgramPlaybackComponent::build_program(EditorialProgramPlaybackInput {
                project_id: "p",
                program_id: "story",
                start_program_frame: 60,
                program: &program,
                covers: &[],
                clips: &[clip("clip_a")],
            })
            .unwrap();

        assert_eq!(request.start_program_frame, FrameNumber(60));
        assert_eq!(request.items.len(), 2);
        let item = &request.items[1];
        assert_eq!(item.record_in_frame, FrameNumber(50));
        assert_eq!(item.record_out_frame, FrameNumber(100));
        assert_eq!(item.sources.len(), 2);
        let video = video_source(item);
        assert_eq!(video.source_ref.in_frame, Some(FrameNumber(200)));
        assert_eq!(video.source_ref.out_frame, Some(FrameNumber(250)));
        assert!(video.has_video);
        assert!(!video.has_audio);
        assert_eq!(video.audio_output_channel, None);
        let audio = audio_source(item, PROGRAM_AUDIO_OUTPUT_CH1);
        assert_eq!(audio.source_ref.in_frame, Some(FrameNumber(200)));
        assert_eq!(audio.source_ref.out_frame, Some(FrameNumber(250)));
        assert!(!audio.has_video);
        assert!(audio.has_audio);
        assert_eq!(audio.audio_output_channel, Some(PROGRAM_AUDIO_OUTPUT_CH1));
    }

    #[test]
    fn contiguous_virtual_ranges_are_one_playlist_line() {
        let program = program(vec![
            EditorialPlaylistSegment {
                part_id: "part_a".into(),
                kind: "tonovi".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 0,
                global_end_frame: 50,
                duration_frames: 50,
                source_in_frame: 100,
                source_out_frame: 150,
                source_fps: 50.0,
                streamable: true,
                ..EditorialPlaylistSegment::default()
            },
            EditorialPlaylistSegment {
                part_id: "part_b".into(),
                kind: "tonovi".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 50,
                global_end_frame: 100,
                duration_frames: 50,
                source_in_frame: 150,
                source_out_frame: 200,
                source_fps: 50.0,
                streamable: true,
                ..EditorialPlaylistSegment::default()
            },
            EditorialPlaylistSegment {
                part_id: "part_c".into(),
                kind: "tonovi".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 100,
                global_end_frame: 150,
                duration_frames: 50,
                source_in_frame: 200,
                source_out_frame: 250,
                source_fps: 50.0,
                streamable: true,
                ..EditorialPlaylistSegment::default()
            },
        ]);

        let request =
            EditorialProgramPlaybackComponent::build_program(EditorialProgramPlaybackInput {
                project_id: "p",
                program_id: "story",
                start_program_frame: 0,
                program: &program,
                covers: &[],
                clips: &[clip("clip_a")],
            })
            .unwrap();

        assert_eq!(request.items.len(), 1);
        let item = &request.items[0];
        assert_eq!(item.record_in_frame, FrameNumber(0));
        assert_eq!(item.record_out_frame, FrameNumber(150));
        assert_eq!(item.sources.len(), 2);
        let video = video_source(item);
        assert_eq!(video.source_ref.in_frame, Some(FrameNumber(100)));
        assert_eq!(video.source_ref.out_frame, Some(FrameNumber(250)));
        assert!(video.has_video);
        assert!(!video.has_audio);
        assert_eq!(video.audio_output_channel, None);
        let audio = audio_source(item, PROGRAM_AUDIO_OUTPUT_CH1);
        assert_eq!(audio.source_ref.in_frame, Some(FrameNumber(100)));
        assert_eq!(audio.source_ref.out_frame, Some(FrameNumber(250)));
        assert!(!audio.has_video);
        assert!(audio.has_audio);
        assert_eq!(audio.audio_output_channel, Some(PROGRAM_AUDIO_OUTPUT_CH1));
    }

    #[test]
    fn off_program_range_is_audio_only_playlist_item() {
        let program = program(vec![EditorialPlaylistSegment {
            part_id: "off_a".into(),
            kind: "offovi".into(),
            clip_id: "clip_a".into(),
            global_start_frame: 0,
            global_end_frame: 50,
            duration_frames: 50,
            source_in_frame: 10,
            source_out_frame: 60,
            source_fps: 50.0,
            streamable: true,
            ..EditorialPlaylistSegment::default()
        }]);

        let request =
            EditorialProgramPlaybackComponent::build_program(EditorialProgramPlaybackInput {
                project_id: "p",
                program_id: "story",
                start_program_frame: 0,
                program: &program,
                covers: &[],
                clips: &[clip("clip_a")],
            })
            .unwrap();

        assert_eq!(request.items.len(), 1);
        assert_eq!(request.items[0].sources.len(), 1);
        let audio = &request.items[0].sources[0];
        assert!(!audio.has_video);
        assert!(audio.has_audio);
        assert_eq!(audio.audio_output_channel, Some(PROGRAM_AUDIO_OUTPUT_CH1));
    }

    #[test]
    fn cover_range_is_one_playlist_item_with_cover_video_a1_a2() {
        let program = program(vec![EditorialPlaylistSegment {
            part_id: "part_a".into(),
            kind: "tonovi".into(),
            clip_id: "clip_a".into(),
            global_start_frame: 0,
            global_end_frame: 50,
            duration_frames: 50,
            source_in_frame: 100,
            source_out_frame: 150,
            source_fps: 50.0,
            streamable: true,
            covers: vec![EditorialPlaylistCover {
                cover_id: "cover_a".into(),
                clip_id: "clip_b".into(),
                virtual_shot_id: "vcover".into(),
                timeline_start_frame: 10,
                timeline_end_frame: 20,
                source_in_frame: 40,
                source_out_frame: 50,
                source_fps: 50.0,
                streamable: true,
                source: Default::default(),
            }],
            ..EditorialPlaylistSegment::default()
        }]);

        let request =
            EditorialProgramPlaybackComponent::build_program(EditorialProgramPlaybackInput {
                project_id: "p",
                program_id: "story",
                start_program_frame: 0,
                program: &program,
                covers: &[],
                clips: &[clip("clip_a"), clip("clip_b")],
            })
            .unwrap();

        assert_eq!(request.items.len(), 3);
        let pre = &request.items[0];
        assert_eq!(pre.record_in_frame, FrameNumber(0));
        assert_eq!(pre.record_out_frame, FrameNumber(10));
        assert_eq!(pre.sources.len(), 2);
        let pre_video = video_source(pre);
        let pre_audio = audio_source(pre, PROGRAM_AUDIO_OUTPUT_CH1);
        assert_eq!(pre_video.source_ref.clip_id, "clip_a");
        assert_eq!(pre_video.source_ref.in_frame, Some(FrameNumber(100)));
        assert_eq!(pre_video.source_ref.out_frame, Some(FrameNumber(110)));
        assert!(pre_video.has_video);
        assert!(!pre_video.has_audio);
        assert_eq!(pre_audio.source_ref.clip_id, "clip_a");
        assert_eq!(pre_audio.source_ref.in_frame, Some(FrameNumber(100)));
        assert_eq!(pre_audio.source_ref.out_frame, Some(FrameNumber(110)));
        assert!(!pre_audio.has_video);
        assert!(pre_audio.has_audio);
        assert_eq!(
            pre_audio.audio_output_channel,
            Some(PROGRAM_AUDIO_OUTPUT_CH1)
        );

        let cover = &request.items[1];
        assert_eq!(cover.record_in_frame, FrameNumber(10));
        assert_eq!(cover.record_out_frame, FrameNumber(20));
        assert_eq!(cover.sources.len(), 2);
        let cover_video = video_source(cover);
        let cover_a1 = audio_source(cover, PROGRAM_AUDIO_OUTPUT_CH1);
        let cover_a2 = audio_source(cover, PROGRAM_AUDIO_OUTPUT_CH2);
        assert_eq!(
            cover
                .sources
                .iter()
                .filter(|source| source.has_video)
                .count(),
            1
        );
        assert_eq!(cover_video.source_ref.clip_id, "clip_b");
        assert_eq!(cover_video.source_ref.in_frame, Some(FrameNumber(40)));
        assert_eq!(cover_video.source_ref.out_frame, Some(FrameNumber(50)));
        assert!(cover_video.has_video);
        assert!(cover_video.has_audio);
        assert_eq!(cover_a1.source_ref.clip_id, "clip_a");
        assert_eq!(cover_a1.source_ref.in_frame, Some(FrameNumber(110)));
        assert_eq!(cover_a1.source_ref.out_frame, Some(FrameNumber(120)));
        assert!(!cover_a1.has_video);
        assert!(cover_a1.has_audio);
        assert_eq!(
            cover_a1.audio_output_channel,
            Some(PROGRAM_AUDIO_OUTPUT_CH1)
        );
        assert_eq!(cover_a2.source_ref.clip_id, "clip_b");
        assert_eq!(cover_a2.source_ref.in_frame, Some(FrameNumber(40)));
        assert_eq!(cover_a2.source_ref.out_frame, Some(FrameNumber(50)));
        assert_eq!(
            cover_a2.audio_output_channel,
            Some(PROGRAM_AUDIO_OUTPUT_CH2)
        );

        let post = &request.items[2];
        assert_eq!(post.record_in_frame, FrameNumber(20));
        assert_eq!(post.record_out_frame, FrameNumber(50));
        assert_eq!(post.sources.len(), 2);
        let post_video = video_source(post);
        let post_audio = audio_source(post, PROGRAM_AUDIO_OUTPUT_CH1);
        assert_eq!(post_video.source_ref.clip_id, "clip_a");
        assert_eq!(post_video.source_ref.in_frame, Some(FrameNumber(120)));
        assert_eq!(post_video.source_ref.out_frame, Some(FrameNumber(150)));
        assert!(post_video.has_video);
        assert!(!post_video.has_audio);
        assert_eq!(post_audio.source_ref.clip_id, "clip_a");
        assert_eq!(post_audio.source_ref.in_frame, Some(FrameNumber(120)));
        assert_eq!(post_audio.source_ref.out_frame, Some(FrameNumber(150)));
        assert!(!post_audio.has_video);
        assert!(post_audio.has_audio);
        assert_eq!(
            post_audio.audio_output_channel,
            Some(PROGRAM_AUDIO_OUTPUT_CH1)
        );
    }

    #[test]
    fn cover_crossing_virtual_boundary_stays_one_overlay_line() {
        let program = program(vec![
            EditorialPlaylistSegment {
                part_id: "part_a".into(),
                kind: "tonovi".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 0,
                global_end_frame: 50,
                duration_frames: 50,
                source_in_frame: 100,
                source_out_frame: 150,
                source_fps: 50.0,
                streamable: true,
                covers: vec![EditorialPlaylistCover {
                    cover_id: "cover_a".into(),
                    clip_id: "clip_b".into(),
                    virtual_shot_id: "vcover".into(),
                    timeline_start_frame: 40,
                    timeline_end_frame: 60,
                    source_in_frame: 10,
                    source_out_frame: 30,
                    source_fps: 50.0,
                    streamable: true,
                    source: Default::default(),
                }],
                ..EditorialPlaylistSegment::default()
            },
            EditorialPlaylistSegment {
                part_id: "part_b".into(),
                kind: "tonovi".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 50,
                global_end_frame: 100,
                duration_frames: 50,
                source_in_frame: 150,
                source_out_frame: 200,
                source_fps: 50.0,
                streamable: true,
                covers: vec![EditorialPlaylistCover {
                    cover_id: "cover_a".into(),
                    clip_id: "clip_b".into(),
                    virtual_shot_id: "vcover".into(),
                    timeline_start_frame: 40,
                    timeline_end_frame: 60,
                    source_in_frame: 10,
                    source_out_frame: 30,
                    source_fps: 50.0,
                    streamable: true,
                    source: Default::default(),
                }],
                ..EditorialPlaylistSegment::default()
            },
        ]);

        let request =
            EditorialProgramPlaybackComponent::build_program(EditorialProgramPlaybackInput {
                project_id: "p",
                program_id: "story",
                start_program_frame: 0,
                program: &program,
                covers: &[],
                clips: &[clip("clip_a"), clip("clip_b")],
            })
            .unwrap();

        assert_eq!(request.items.len(), 3);
        let cover = &request.items[1];
        assert_eq!(cover.record_in_frame, FrameNumber(40));
        assert_eq!(cover.record_out_frame, FrameNumber(60));
        assert_eq!(cover.sources.len(), 2);
        let a1 = audio_source(cover, PROGRAM_AUDIO_OUTPUT_CH1);
        let a2 = audio_source(cover, PROGRAM_AUDIO_OUTPUT_CH2);
        let video = video_source(cover);
        assert_eq!(
            cover
                .sources
                .iter()
                .filter(|source| source.has_video)
                .count(),
            1
        );
        assert_eq!(a1.source_ref.clip_id, "clip_a");
        assert_eq!(a1.source_ref.in_frame, Some(FrameNumber(140)));
        assert_eq!(a1.source_ref.out_frame, Some(FrameNumber(160)));
        assert!(!a1.has_video);
        assert_eq!(a2.source_ref.clip_id, "clip_b");
        assert_eq!(a2.source_ref.in_frame, Some(FrameNumber(10)));
        assert_eq!(a2.source_ref.out_frame, Some(FrameNumber(30)));
        assert_eq!(video.source_ref.clip_id, "clip_b");
        assert_eq!(video.source_ref.out_frame, Some(FrameNumber(30)));
    }

    #[test]
    fn cover_take_keeps_cover_source_fps() {
        let program = program(vec![EditorialPlaylistSegment {
            part_id: "part_a".into(),
            kind: "tonovi".into(),
            clip_id: "clip_a".into(),
            global_start_frame: 0,
            global_end_frame: 50,
            duration_frames: 50,
            source_in_frame: 100,
            source_out_frame: 150,
            source_fps: 50.0,
            streamable: true,
            covers: vec![EditorialPlaylistCover {
                cover_id: "cover_fast".into(),
                clip_id: "clip_b".into(),
                virtual_shot_id: "vcover".into(),
                timeline_start_frame: 10,
                timeline_end_frame: 20,
                source_in_frame: 40,
                source_out_frame: 80,
                source_fps: 59.94,
                streamable: true,
                source: Default::default(),
            }],
            ..EditorialPlaylistSegment::default()
        }]);

        let request =
            EditorialProgramPlaybackComponent::build_program(EditorialProgramPlaybackInput {
                project_id: "p",
                program_id: "story",
                start_program_frame: 0,
                program: &program,
                covers: &[],
                clips: &[clip("clip_a"), clip("clip_b")],
            })
            .unwrap();

        let cover_video = video_source(&request.items[1]);
        assert_eq!(cover_video.source_ref.clip_id, "clip_b");
        assert_eq!(cover_video.source_fps, 59.94);
        assert_eq!(cover_video.source_ref.in_frame, Some(FrameNumber(40)));
    }
}
