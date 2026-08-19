use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::editor_assets::ensure_virtual_stream_cached;
use crate::frame_time::{frame_to_seconds, is_valid_fps, seconds_to_frame};
use crate::media::resolve_play_media;
use crate::media_pool::resolve_clip_fps;
use crate::project::db::ProjectPaths;

use super::db::{cover_stream_frames, part_stream_frames};
use super::playlist::{
    build_editorial_playlist, EditorialCover, EditorialPlaylist, EditorialSegment,
};

const EPS: f64 = 0.001;

/// Kodak audio bus roles (perforacije). MVP: A1 + A2.
/// TODO: `project.settings.audio.max_channels` (1–4) — see docs/qnc-playback-engine.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioBusRole {
    A1,
    A2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioBus {
    pub index: u8,
    pub role: AudioBusRole,
}

/// Fixed MVP buses; expand when project max_channels lands.
pub const PLAYBACK_AUDIO_BUSES: [AudioBus; 2] = [
    AudioBus {
        index: 0,
        role: AudioBusRole::A1,
    },
    AudioBus {
        index: 1,
        role: AudioBusRole::A2,
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct PlaybackClock {
    pub virtual_frame: i64,
    pub virtual_sec: f64,
    pub playing: bool,
    pub paused: bool,
    #[serde(skip)]
    pub last_tick: Instant,
}

#[derive(Debug, Clone)]
struct PartStreamRef {
    /// Mux V+A for izjava-ton (empty for off).
    mux_stream_url: String,
    /// A1 audio-only part stream (always set when streamable).
    a1_stream_url: String,
    has_audio: bool,
    audio_channels: u8,
}

#[derive(Debug, Clone)]
struct CoverStreamRef {
    stream_url: String,
    clip_id: String,
    in_frame: i64,
    out_frame: i64,
    fps: f64,
    has_audio: bool,
    audio_channels: u8,
}

/// All-tab / source dock — one clip, clock = source seconds (Rust frame + audio).
#[derive(Debug, Clone, Serialize)]
pub struct SourceClipPlayback {
    pub clip_id: String,
    pub in_frame: i64,
    pub out_frame: i64,
    pub in_sec: f64,
    pub out_sec: f64,
    pub fps: f64,
    pub has_audio: bool,
    pub audio_channels: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaybackSession {
    pub session_id: String,
    pub project_id: String,
    pub playlist: EditorialPlaylist,
    pub clock: PlaybackClock,
    pub preload: HashMap<String, CoverPreload>,
    /// When set, preview ignores wrap playlist and plays this clip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_clip: Option<SourceClipPlayback>,
    #[serde(skip)]
    part_streams: HashMap<String, PartStreamRef>,
    #[serde(skip)]
    cover_streams: HashMap<String, CoverStreamRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ActiveLayerKind {
    Part,
    Cover,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveLayer {
    pub layer: ActiveLayerKind,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub part_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cover_id: String,
    /// Source/All clip when session.source_clip is set.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub clip_id: String,
    /// Primary video stream (cover in slot, mux part for ton, empty for off outside cover).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stream_url: String,
    /// A1 kostur — audio-only part stream whenever segment is active.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub a1_stream_url: String,
    /// Program audio URL. Legacy field name; payload is A1/A2 dual-mono, not a mix.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mixed_audio_url: String,
    /// Preview kadar (JPEG) za aktivni video sloj.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub preview_frame_url: String,
    #[serde(default)]
    pub part_kind: String,
    #[serde(default)]
    pub video_blank: bool,
    pub has_video: bool,
    pub has_audio: bool,
    pub audio_channels: u8,
    pub local_frame: i64,
    pub source_frame: i64,
    pub local_sec: f64,
    pub source_sec: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackState {
    pub session_id: String,
    pub virtual_frame: i64,
    pub virtual_sec: f64,
    pub playing: bool,
    pub paused: bool,
    /// Session clock rate. Source preview uses the resolved source/proxy FPS;
    /// wrap playback uses the project timeline FPS.
    pub timebase_fps: f64,
    pub active: ActiveLayer,
    pub cover_preload: Vec<CoverPreload>,
    /// Kodak A1/A2 (MVP). Expand when project max_channels lands.
    pub audio_buses: Vec<AudioBus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CoverPreloadStatus {
    Pending,
    Generating,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverPreload {
    pub cover_id: String,
    pub status: CoverPreloadStatus,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

#[derive(Clone, Default)]
pub struct PlaybackStore {
    inner: Arc<Mutex<HashMap<String, PlaybackSession>>>,
}

impl PlaybackStore {
    pub fn start(&self, paths: &ProjectPaths, project_id: &str) -> Result<PlaybackState, String> {
        let pid = project_id.trim();
        if pid.is_empty() {
            return Err("project_id required".into());
        }
        let playlist = build_editorial_playlist(paths, pid)?;
        let (part_streams, cover_streams) = freeze_stream_refs(paths, pid, &playlist)?;
        let session_id = format!("story_play_{}", uuid::Uuid::new_v4().simple());
        let clock = PlaybackClock {
            virtual_frame: 0,
            virtual_sec: 0.0,
            playing: false,
            paused: true,
            last_tick: Instant::now(),
        };
        let mut preload = HashMap::new();
        for segment in &playlist.segments {
            for cover in &segment.covers {
                if cover.streamable {
                    preload.insert(
                        cover.cover_id.clone(),
                        CoverPreload {
                            cover_id: cover.cover_id.clone(),
                            status: CoverPreloadStatus::Pending,
                            error: String::new(),
                        },
                    );
                }
            }
        }
        let session = PlaybackSession {
            session_id: session_id.clone(),
            project_id: pid.to_string(),
            playlist,
            clock,
            preload,
            source_clip: None,
            part_streams,
            cover_streams,
        };
        let mut map = self
            .inner
            .lock()
            .map_err(|_| "playback store lock".to_string())?;
        map.insert(session_id.clone(), session.clone());
        drop(map);
        self.spawn_preload(paths.clone(), pid.to_string(), session_id.clone());
        Ok(state_from_session(&session))
    }

    /// All/source preview legacy adapter: converts seconds to source frames.
    pub fn start_source(
        &self,
        paths: &ProjectPaths,
        project_id: &str,
        clip_id: &str,
        in_sec: Option<f64>,
        out_sec: Option<f64>,
    ) -> Result<PlaybackState, String> {
        let pid = project_id.trim();
        let clip = clip_id.trim();
        if pid.is_empty() {
            return Err("project_id required".into());
        }
        if clip.is_empty() {
            return Err("clip_id required".into());
        }
        let metadata = clip_playback_metadata(paths, pid, clip)?;
        let in_s = in_sec.unwrap_or(0.0).max(0.0);
        let out_s = out_sec
            .unwrap_or(metadata.duration_sec)
            .max(in_s + EPS)
            .min(if metadata.duration_sec > EPS {
                metadata.duration_sec
            } else {
                f64::MAX
            });
        let in_frame = seconds_to_frame(in_s, metadata.fps);
        let out_frame = seconds_to_frame(out_s, metadata.fps).max(in_frame + 1);
        self.start_source_frames(paths, pid, clip, in_frame, out_frame)
    }

    /// All/source preview: one clip, clock = source frame on the proxy.
    pub fn start_source_frames(
        &self,
        paths: &ProjectPaths,
        project_id: &str,
        clip_id: &str,
        in_frame: i64,
        out_frame: i64,
    ) -> Result<PlaybackState, String> {
        let pid = project_id.trim();
        let clip = clip_id.trim();
        if pid.is_empty() {
            return Err("project_id required".into());
        }
        if clip.is_empty() {
            return Err("clip_id required".into());
        }
        let metadata = clip_playback_metadata(paths, pid, clip)?;
        self.start_source_resolved(paths, pid, clip, in_frame, out_frame, &metadata)
    }

    /// Source preview from the editorial source of truth: virtual_shots row.
    /// The UI may select/scrub, but durable clip/in/out/fps come from SQLite.
    pub fn start_virtual_shot(
        &self,
        paths: &ProjectPaths,
        project_id: &str,
        virtual_shot_id: &str,
    ) -> Result<PlaybackState, String> {
        let pid = project_id.trim();
        let shot_id = virtual_shot_id.trim();
        if pid.is_empty() {
            return Err("project_id required".into());
        }
        if shot_id.is_empty() {
            return Err("virtual_shot_id required".into());
        }
        let (clip_id, in_frame, out_frame, fps) =
            crate::virtual_shots::virtual_shot_frames(paths, pid, shot_id)?;
        let mut metadata = clip_playback_metadata(paths, pid, &clip_id)?;
        metadata.fps = fps;
        self.start_source_resolved(paths, pid, &clip_id, in_frame, out_frame, &metadata)
    }

    fn start_source_resolved(
        &self,
        paths: &ProjectPaths,
        project_id: &str,
        clip_id: &str,
        in_frame: i64,
        out_frame: i64,
        metadata: &ClipPlaybackMetadata,
    ) -> Result<PlaybackState, String> {
        let pid = project_id.trim();
        let clip = clip_id.trim();
        if pid.is_empty() {
            return Err("project_id required".into());
        }
        if clip.is_empty() {
            return Err("clip_id required".into());
        }
        let _ = resolve_play_media(paths, pid, clip)?;
        if !metadata.fps.is_finite() || metadata.fps <= 0.0 {
            return Err(format!(
                "clip_fps_invalid: '{clip}' nema valjan source FPS za playback"
            ));
        }
        let duration_frame = seconds_to_frame(metadata.duration_sec.max(0.0), metadata.fps).max(1);
        let in_frame = in_frame.max(0).min(duration_frame.saturating_sub(1));
        let out_frame = out_frame.max(in_frame + 1).min(duration_frame);
        let in_s = frame_to_seconds(in_frame, metadata.fps);
        let out_s = frame_to_seconds(out_frame, metadata.fps).max(in_s + EPS);
        let playlist = EditorialPlaylist {
            project_id: pid.to_string(),
            timeline_fps: metadata.fps,
            duration_frames: (out_frame - in_frame).max(0),
            duration_sec: (out_s - in_s).max(0.0),
            segments: Vec::new(),
        };
        let session_id = format!("story_src_{}", uuid::Uuid::new_v4().simple());
        let session = PlaybackSession {
            session_id: session_id.clone(),
            project_id: pid.to_string(),
            playlist,
            clock: PlaybackClock {
                virtual_frame: in_frame,
                virtual_sec: in_s,
                playing: false,
                paused: true,
                last_tick: Instant::now(),
            },
            preload: HashMap::new(),
            source_clip: Some(SourceClipPlayback {
                clip_id: clip.to_string(),
                in_frame,
                out_frame,
                in_sec: in_s,
                out_sec: out_s,
                fps: metadata.fps,
                has_audio: metadata.has_audio,
                audio_channels: metadata.audio_channels,
            }),
            part_streams: HashMap::new(),
            cover_streams: HashMap::new(),
        };
        let mut map = self
            .inner
            .lock()
            .map_err(|_| "playback store lock".to_string())?;
        map.insert(session_id, session.clone());
        Ok(state_from_session(&session))
    }

    pub fn stop(&self, session_id: &str) -> Result<(), String> {
        let sid = session_id.trim();
        if sid.is_empty() {
            return Ok(());
        }
        let mut map = self
            .inner
            .lock()
            .map_err(|_| "playback store lock".to_string())?;
        map.remove(sid);
        Ok(())
    }

    pub fn seek_frame(&self, session_id: &str, virtual_frame: i64) -> Result<(), String> {
        let sid = session_id.trim();
        let mut map = self
            .inner
            .lock()
            .map_err(|_| "playback store lock".to_string())?;
        let session = map
            .get_mut(sid)
            .ok_or_else(|| format!("playback session not found: {sid}"))?;
        let frame = clamp_session_frame(session, virtual_frame)?;
        session.clock.virtual_frame = frame;
        session.clock.virtual_sec = frame_to_seconds(frame, session_timebase_fps(session)?);
        session.clock.last_tick = Instant::now();
        Ok(())
    }

    pub fn pause(&self, session_id: &str, paused: bool) -> Result<(), String> {
        let sid = session_id.trim();
        let mut map = self
            .inner
            .lock()
            .map_err(|_| "playback store lock".to_string())?;
        let session = map
            .get_mut(sid)
            .ok_or_else(|| format!("playback session not found: {sid}"))?;
        let now = Instant::now();
        advance_session_clock(session, now)?;
        session.clock.paused = paused;
        session.clock.playing = !paused;
        session.clock.last_tick = now;
        Ok(())
    }

    pub fn state(&self, _paths: &ProjectPaths, session_id: &str) -> Result<PlaybackState, String> {
        let sid = session_id.trim();
        let mut map = self
            .inner
            .lock()
            .map_err(|_| "playback store lock".to_string())?;
        let session = map
            .get_mut(sid)
            .ok_or_else(|| format!("playback session not found: {sid}"))?;
        advance_session_clock(session, Instant::now())?;
        Ok(state_from_session(session))
    }

    pub fn session(&self, session_id: &str) -> Result<PlaybackSession, String> {
        let sid = session_id.trim();
        let map = self
            .inner
            .lock()
            .map_err(|_| "playback store lock".to_string())?;
        map.get(sid)
            .cloned()
            .ok_or_else(|| format!("playback session not found: {sid}"))
    }

    pub fn spawn_preload(&self, paths: ProjectPaths, project_id: String, session_id: String) {
        let store = self.clone();
        tokio::spawn(async move {
            let cover_streams = {
                let map = match store.inner.lock() {
                    Ok(m) => m,
                    Err(_) => return,
                };
                let session = match map.get(session_id.trim()) {
                    Some(s) => s,
                    None => return,
                };
                session.cover_streams.clone()
            };

            for (cover_id, frozen) in cover_streams {
                {
                    let mut map = match store.inner.lock() {
                        Ok(m) => m,
                        Err(_) => return,
                    };
                    if let Some(session) = map.get_mut(session_id.trim()) {
                        if let Some(entry) = session.preload.get_mut(&cover_id) {
                            entry.status = CoverPreloadStatus::Generating;
                            entry.error.clear();
                        }
                    }
                }

                let res = ensure_virtual_stream_cached(
                    &paths,
                    &project_id,
                    &frozen.clip_id,
                    frozen.in_frame,
                    frozen.out_frame,
                    frozen.fps,
                )
                .await
                .map(|_| ());

                let mut map = match store.inner.lock() {
                    Ok(m) => m,
                    Err(_) => return,
                };
                if let Some(session) = map.get_mut(session_id.trim()) {
                    if let Some(entry) = session.preload.get_mut(&cover_id) {
                        match res {
                            Ok(_) => {
                                entry.status = CoverPreloadStatus::Ready;
                                entry.error.clear();
                            }
                            Err(err) => {
                                entry.status = CoverPreloadStatus::Failed;
                                entry.error = err;
                            }
                        }
                    }
                }
            }
        });
    }
}

fn state_from_session(session: &PlaybackSession) -> PlaybackState {
    let virtual_frame = session.clock.virtual_frame.max(0);
    let timebase_fps =
        session_timebase_fps(session).unwrap_or(session.playlist.timeline_fps.max(1.0));
    let virtual_sec = frame_to_seconds(virtual_frame, timebase_fps);
    let mut active = resolve_active_layer_frozen(session, virtual_frame);
    let sid = session.session_id.clone();
    active.mixed_audio_url = format!(
        "/api/story/playback/audio?session_id={}&duration_frames={}",
        url_encode_query_value(&sid),
        seconds_to_frame(30.0, timebase_fps).max(1)
    );
    active.preview_frame_url = format!(
        "/api/story/playback/frame?session_id={}&virtual_frame={virtual_frame}",
        url_encode_query_value(&sid)
    );
    let cover_preload = session.preload.values().cloned().collect::<Vec<_>>();
    PlaybackState {
        session_id: session.session_id.clone(),
        virtual_frame,
        virtual_sec,
        playing: session.clock.playing,
        paused: session.clock.paused,
        timebase_fps: session.playlist.timeline_fps,
        active,
        cover_preload,
        audio_buses: PLAYBACK_AUDIO_BUSES.to_vec(),
    }
}

fn freeze_stream_refs(
    paths: &ProjectPaths,
    project_id: &str,
    playlist: &EditorialPlaylist,
) -> Result<
    (
        HashMap<String, PartStreamRef>,
        HashMap<String, CoverStreamRef>,
    ),
    String,
> {
    let pid_enc = url_encode_project(project_id);
    let mut part_streams = HashMap::new();
    let mut cover_streams = HashMap::new();
    for segment in &playlist.segments {
        if segment.streamable {
            let (clip_id, _, _, _) = part_stream_frames(paths, project_id, &segment.part_id)?;
            // Proxy-first: session start fails hard if play media is missing.
            let _ = resolve_play_media(paths, project_id, &segment.clip_id)?;
            let metadata = clip_playback_metadata(paths, project_id, &clip_id)?;
            let is_off = segment.kind.trim().eq_ignore_ascii_case("offovi");
            let mux = if is_off {
                String::new()
            } else {
                format!(
                    "/api/story/virtual-stream?project_id={pid_enc}&part_id={}",
                    segment.part_id
                )
            };
            let a1 = format!(
                "/api/story/virtual-stream?project_id={pid_enc}&part_id={}&audio_only=1",
                segment.part_id
            );
            part_streams.insert(
                segment.part_id.clone(),
                PartStreamRef {
                    mux_stream_url: mux,
                    a1_stream_url: a1,
                    has_audio: metadata.has_audio,
                    audio_channels: metadata.audio_channels,
                },
            );
        }
        for cover in &segment.covers {
            if !cover.streamable {
                continue;
            }
            let (clip_id, in_frame, out_frame, fps) =
                cover_stream_frames(paths, project_id, &cover.cover_id)?;
            let _ = resolve_play_media(paths, project_id, &clip_id)?;
            let metadata = clip_playback_metadata(paths, project_id, &clip_id)?;
            cover_streams.insert(
                cover.cover_id.clone(),
                CoverStreamRef {
                    stream_url: format!(
                        "/api/story/virtual-stream?project_id={pid_enc}&cover_id={}",
                        cover.cover_id
                    ),
                    clip_id,
                    in_frame,
                    out_frame,
                    fps,
                    has_audio: metadata.has_audio,
                    audio_channels: metadata.audio_channels,
                },
            );
        }
    }
    Ok((part_streams, cover_streams))
}

pub(crate) fn resolve_active_layer_frame_public(
    session: &PlaybackSession,
    virtual_frame: i64,
) -> ActiveLayer {
    resolve_active_layer_frozen(session, virtual_frame)
}

fn resolve_active_layer_frozen(session: &PlaybackSession, virtual_frame: i64) -> ActiveLayer {
    let v = virtual_frame.max(0);
    let timeline_fps =
        session_timebase_fps(session).unwrap_or(session.playlist.timeline_fps.max(1.0));
    if let Some(src) = session.source_clip.as_ref() {
        let source_frame = v.clamp(src.in_frame, src.out_frame.max(src.in_frame));
        let source_sec = frame_to_seconds(source_frame, src.fps);
        let local_frame = (source_frame - src.in_frame).max(0);
        let pid_enc = url_encode_project(&session.project_id);
        let clip_enc = url_encode_query_value(&src.clip_id);
        return ActiveLayer {
            layer: ActiveLayerKind::Part,
            part_id: String::new(),
            cover_id: String::new(),
            clip_id: src.clip_id.clone(),
            stream_url: format!(
                "/api/story/virtual-stream?project_id={pid_enc}&clip_id={clip_enc}&in_frame={}&out_frame={}",
                src.in_frame, src.out_frame
            ),
            a1_stream_url: format!(
                "/api/story/virtual-stream?project_id={pid_enc}&clip_id={clip_enc}&in_frame={}&out_frame={}&audio_only=1",
                src.in_frame, src.out_frame
            ),
            mixed_audio_url: String::new(),
            preview_frame_url: String::new(),
            part_kind: "source".into(),
            video_blank: false,
            has_video: true,
            has_audio: src.has_audio,
            audio_channels: src.audio_channels,
            local_frame,
            source_frame,
            local_sec: frame_to_seconds(local_frame, src.fps),
            source_sec,
        };
    }
    let (segment, local_frame) = find_segment_frame(&session.playlist, v);
    let Some(segment) = segment else {
        return ActiveLayer {
            layer: ActiveLayerKind::None,
            part_id: String::new(),
            cover_id: String::new(),
            clip_id: String::new(),
            stream_url: String::new(),
            a1_stream_url: String::new(),
            mixed_audio_url: String::new(),
            preview_frame_url: String::new(),
            part_kind: String::new(),
            video_blank: true,
            has_video: false,
            has_audio: false,
            audio_channels: 0,
            local_frame: 0,
            source_frame: 0,
            local_sec: 0.0,
            source_sec: 0.0,
        };
    };
    let is_off = segment.kind.trim().eq_ignore_ascii_case("offovi");
    let part_ref = session.part_streams.get(&segment.part_id);
    let a1_url = part_ref
        .map(|row| row.a1_stream_url.clone())
        .unwrap_or_default();
    let mux_url = part_ref
        .map(|row| row.mux_stream_url.clone())
        .unwrap_or_default();
    let part_has_audio = part_ref.map(|row| row.has_audio).unwrap_or(false);
    let part_audio_channels = part_ref.map(|row| row.audio_channels).unwrap_or(0);

    if let Some(cover) = find_cover_frame(&segment.covers, v) {
        let cover_ref = session.cover_streams.get(&cover.cover_id);
        let stream_url = cover_ref
            .map(|row| row.stream_url.clone())
            .unwrap_or_default();
        let cover_has_audio = cover_ref.map(|row| row.has_audio).unwrap_or(false);
        let cover_audio_channels = cover_ref.map(|row| row.audio_channels).unwrap_or(0);
        let has_video = !stream_url.trim().is_empty();
        let source_fps = if is_valid_fps(cover.source_fps) {
            cover.source_fps
        } else {
            cover_ref.map(|row| row.fps).unwrap_or(timeline_fps)
        };
        let source_frame = cover_source_offset_frame(cover, v, timeline_fps, source_fps).max(0);
        let source_sec = frame_to_seconds(source_frame, source_fps);
        return ActiveLayer {
            layer: ActiveLayerKind::Cover,
            part_id: segment.part_id.clone(),
            cover_id: cover.cover_id.clone(),
            clip_id: String::new(),
            stream_url,
            a1_stream_url: a1_url,
            mixed_audio_url: String::new(),
            preview_frame_url: String::new(),
            part_kind: segment.kind.clone(),
            video_blank: false,
            has_video,
            has_audio: part_has_audio || cover_has_audio,
            audio_channels: part_audio_channels.max(cover_audio_channels),
            local_frame,
            source_frame,
            local_sec: frame_to_seconds(local_frame, timeline_fps),
            source_sec,
        };
    }

    if is_off {
        return ActiveLayer {
            layer: ActiveLayerKind::Part,
            part_id: segment.part_id.clone(),
            cover_id: String::new(),
            clip_id: String::new(),
            stream_url: String::new(),
            a1_stream_url: a1_url,
            mixed_audio_url: String::new(),
            preview_frame_url: String::new(),
            part_kind: segment.kind.clone(),
            video_blank: true,
            has_video: false,
            has_audio: part_has_audio,
            audio_channels: part_audio_channels,
            local_frame,
            source_frame: local_frame,
            local_sec: frame_to_seconds(local_frame, timeline_fps),
            source_sec: frame_to_seconds(local_frame, timeline_fps),
        };
    }

    ActiveLayer {
        layer: ActiveLayerKind::Part,
        part_id: segment.part_id.clone(),
        cover_id: String::new(),
        clip_id: String::new(),
        stream_url: mux_url.clone(),
        a1_stream_url: a1_url,
        mixed_audio_url: String::new(),
        preview_frame_url: String::new(),
        part_kind: segment.kind.clone(),
        video_blank: false,
        has_video: !mux_url.trim().is_empty(),
        has_audio: part_has_audio,
        audio_channels: part_audio_channels,
        local_frame,
        source_frame: local_frame,
        local_sec: frame_to_seconds(local_frame, timeline_fps),
        source_sec: frame_to_seconds(local_frame, timeline_fps),
    }
}

pub(crate) fn source_offset_for_record_frame(
    record_offset_frame: i64,
    _timeline_fps: f64,
    _source_fps: f64,
) -> i64 {
    record_offset_frame.max(0)
}

fn cover_source_offset_frame(
    cover: &EditorialCover,
    virtual_frame: i64,
    timeline_fps: f64,
    source_fps: f64,
) -> i64 {
    let record_offset = virtual_frame
        .max(0)
        .saturating_sub(cover.timeline_start_frame.max(0));
    let source_offset = source_offset_for_record_frame(record_offset, timeline_fps, source_fps);
    let source_span = (cover.source_out_frame - cover.source_in_frame).max(1);
    source_offset.clamp(0, source_span.saturating_sub(1))
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ClipPlaybackMetadata {
    duration_sec: f64,
    fps: f64,
    has_audio: bool,
    audio_channels: u8,
}

fn clip_playback_metadata(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<ClipPlaybackMetadata, String> {
    use crate::ingest::db::open_ingest;
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    let db_row: Result<(f64, i64, i64), _> = conn.query_row(
        "SELECT COALESCE(duration_sec, 0),
                COALESCE(has_audio, 0),
                COALESCE(audio_channels, 0)
         FROM ingest_assets
         WHERE clip_id = ?1
         ORDER BY CASE import_status WHEN 'imported' THEN 0 WHEN 'done' THEN 1 ELSE 2 END
         LIMIT 1",
        rusqlite::params![clip_id.trim()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    );
    let (duration_sec, has_audio, audio_channels) =
        db_row.map_err(|_| format!("clip_missing: '{clip_id}' nije u ingest_assets"))?;
    let fps = resolve_clip_fps(paths, project_id, clip_id)?;
    let audio_channels = if has_audio != 0 {
        (audio_channels as u8).clamp(1, 4)
    } else {
        0
    };
    Ok(ClipPlaybackMetadata {
        duration_sec,
        fps,
        has_audio: has_audio != 0 && audio_channels > 0,
        audio_channels,
    })
}

pub(crate) fn find_segment_frame<'a>(
    playlist: &'a EditorialPlaylist,
    virtual_frame: i64,
) -> (Option<&'a EditorialSegment>, i64) {
    for segment in &playlist.segments {
        let start = segment.global_start_frame.max(0);
        let end = segment.global_end_frame.max(start + 1);
        if virtual_frame >= start && virtual_frame < end {
            let local_frame = (virtual_frame - start).max(0);
            return (Some(segment), local_frame);
        }
    }
    (None, 0)
}

fn url_encode_project(project_id: &str) -> String {
    url_encode_query_value(project_id)
}

fn url_encode_query_value(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

pub(crate) fn find_cover_frame<'a>(
    covers: &'a [EditorialCover],
    virtual_frame: i64,
) -> Option<&'a EditorialCover> {
    for cover in covers {
        if !cover.streamable {
            continue;
        }
        let start = cover.timeline_start_frame.max(0);
        let end = cover.timeline_end_frame.max(start + 1);
        if virtual_frame >= start && virtual_frame < end {
            return Some(cover);
        }
    }
    None
}

fn session_end_frame(session: &PlaybackSession) -> i64 {
    if let Some(src) = session.source_clip.as_ref() {
        src.out_frame.max(src.in_frame)
    } else {
        session.playlist.duration_frames.max(0)
    }
}

fn session_timebase_fps(session: &PlaybackSession) -> Result<f64, String> {
    if let Some(src) = session.source_clip.as_ref() {
        if src.fps.is_finite() && src.fps > 0.0 {
            return Ok(src.fps);
        }
        return Err(format!(
            "clip_fps_invalid: '{}' nema valjan source FPS za playback",
            src.clip_id
        ));
    }
    let fps = session.playlist.timeline_fps;
    if fps.is_finite() && fps > 0.0 {
        Ok(fps)
    } else {
        Err("timeline_fps_invalid: projekt nema valjan timeline FPS za playback".into())
    }
}

fn clamp_session_frame(session: &PlaybackSession, virtual_frame: i64) -> Result<i64, String> {
    let mut frame = virtual_frame.max(0);
    if let Some(src) = session.source_clip.as_ref() {
        frame = frame.clamp(src.in_frame, src.out_frame.max(src.in_frame));
    } else {
        let end = session_end_frame(session);
        if end > 0 {
            frame = frame.min(end);
        }
    }
    Ok(frame.max(0))
}

fn advance_session_clock(session: &mut PlaybackSession, now: Instant) -> Result<(), String> {
    if !session.clock.playing || session.clock.paused {
        session.clock.last_tick = now;
        return Ok(());
    }

    let dt = now
        .saturating_duration_since(session.clock.last_tick)
        .as_secs_f64();
    if dt <= 0.0 {
        return Ok(());
    }

    let fps = session_timebase_fps(session)?;
    let end = session_end_frame(session);
    let raw_next_sec = frame_to_seconds(session.clock.virtual_frame.max(0), fps) + dt;
    let mut next_frame = seconds_to_frame(raw_next_sec, fps);
    let reached_end = end > 0 && next_frame >= end;
    next_frame = if reached_end {
        end
    } else {
        clamp_session_frame(session, next_frame)?
    };
    if next_frame == session.clock.virtual_frame {
        return Ok(());
    }
    session.clock.virtual_frame = next_frame;
    session.clock.virtual_sec = frame_to_seconds(next_frame, fps);
    session.clock.last_tick = now;
    if reached_end {
        session.clock.playing = false;
        session.clock.paused = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::db::ProjectPaths;
    use crate::story::playlist::{EditorialCover, EditorialPlaylist, EditorialSegment, StreamRef};

    fn test_paths(base: &std::path::Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: std::path::PathBuf::from("nonexistent"),
        }
    }

    fn playlist_one() -> EditorialPlaylist {
        EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 25.0,
            duration_frames: 250,
            duration_sec: 10.0,
            segments: vec![EditorialSegment {
                part_id: "part_a".into(),
                kind: "tonovi".into(),
                title: "".into(),
                clip_id: "clip_a".into(),
                virtual_shot_id: "shot_a".into(),
                global_start_sec: 0.0,
                global_end_sec: 10.0,
                global_start_frame: 0,
                global_end_frame: 250,
                duration_frames: 250,
                duration_sec: 10.0,
                source_in_frame: 0,
                source_out_frame: 250,
                source_fps: 25.0,
                streamable: true,
                source: StreamRef::Part {
                    part_id: "part_a".into(),
                },
                covers: vec![EditorialCover {
                    cover_id: "cover_a".into(),
                    clip_id: "clip_a".into(),
                    virtual_shot_id: "shot_a".into(),
                    title: "".into(),
                    timeline_start_sec: 2.0,
                    timeline_end_sec: 4.0,
                    timeline_start_frame: 50,
                    timeline_end_frame: 100,
                    local_start_sec: 2.0,
                    local_end_sec: 4.0,
                    local_start_frame: 50,
                    local_end_frame: 100,
                    slot_duration_frames: 50,
                    slot_duration_sec: 2.0,
                    source_in_frame: 25,
                    source_out_frame: 75,
                    source_fps: 25.0,
                    source_offset_frames: 0,
                    source_in_sec: 1.0,
                    source_out_sec: 3.0,
                    source_offset_sec: 0.0,
                    streamable: true,
                    stream_error: "".into(),
                    source: StreamRef::Cover {
                        cover_id: "cover_a".into(),
                    },
                    preload_lead_sec: 6.0,
                }],
            }],
        }
    }

    fn frozen_session(playlist: EditorialPlaylist) -> PlaybackSession {
        let mut part_streams = HashMap::new();
        part_streams.insert(
            "part_a".into(),
            PartStreamRef {
                mux_stream_url: "/api/story/virtual-stream?project_id=p&part_id=part_a".into(),
                a1_stream_url: "/api/story/virtual-stream?project_id=p&part_id=part_a&audio_only=1"
                    .into(),
                has_audio: true,
                audio_channels: 2,
            },
        );
        let mut cover_streams = HashMap::new();
        cover_streams.insert(
            "cover_a".into(),
            CoverStreamRef {
                stream_url: "/api/story/virtual-stream?project_id=p&cover_id=cover_a".into(),
                clip_id: "clip_a".into(),
                in_frame: 25,
                out_frame: 75,
                fps: 25.0,
                has_audio: true,
                audio_channels: 2,
            },
        );
        PlaybackSession {
            session_id: "sess".into(),
            project_id: "p".into(),
            playlist,
            clock: PlaybackClock {
                virtual_frame: 0,
                virtual_sec: 0.0,
                playing: true,
                paused: false,
                last_tick: Instant::now(),
            },
            preload: HashMap::new(),
            source_clip: None,
            part_streams,
            cover_streams,
        }
    }

    #[test]
    fn playback_clock_advances_inside_session_state() {
        let mut session = frozen_session(playlist_one());
        let now = Instant::now();
        session.clock.virtual_frame = 25;
        session.clock.virtual_sec = 1.0;
        session.clock.playing = true;
        session.clock.paused = false;
        session.clock.last_tick = now - std::time::Duration::from_millis(80);

        advance_session_clock(&mut session, now).unwrap();

        assert_eq!(session.clock.virtual_sec, 1.08);
        assert!(session.clock.playing);
        assert!(!session.clock.paused);
    }

    #[test]
    fn source_clock_uses_source_fps_not_project_default() {
        let mut session = frozen_session(EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 25.0,
            duration_frames: 200,
            duration_sec: 4.0,
            segments: Vec::new(),
        });
        session.source_clip = Some(SourceClipPlayback {
            clip_id: "clip_a".into(),
            in_frame: 50,
            out_frame: 250,
            in_sec: 1.0,
            out_sec: 5.0,
            fps: 50.0,
            has_audio: true,
            audio_channels: 2,
        });
        let now = Instant::now();
        session.clock.virtual_frame = 50;
        session.clock.virtual_sec = 1.0;
        session.clock.playing = true;
        session.clock.paused = false;
        session.clock.last_tick = now - std::time::Duration::from_millis(40);

        advance_session_clock(&mut session, now).unwrap();

        assert_eq!(session.clock.virtual_sec, 1.04);
    }

    #[test]
    fn playback_clock_stops_at_session_end() {
        let mut session = frozen_session(playlist_one());
        let now = Instant::now();
        session.clock.virtual_frame = 249;
        session.clock.virtual_sec = 9.98;
        session.clock.playing = true;
        session.clock.paused = false;
        session.clock.last_tick = now - std::time::Duration::from_millis(120);

        advance_session_clock(&mut session, now).unwrap();

        assert_eq!(session.clock.virtual_sec, 10.0);
        assert!(!session.clock.playing);
        assert!(session.clock.paused);
    }

    #[test]
    fn state_exposes_kodak_a1_a2_audio_buses() {
        let session = frozen_session(playlist_one());
        let state = state_from_session(&session);
        assert_eq!(state.audio_buses.len(), 2);
        assert_eq!(state.audio_buses[0].role, AudioBusRole::A1);
        assert_eq!(state.audio_buses[1].role, AudioBusRole::A2);
        assert_eq!(state.timebase_fps, 25.0);
    }

    #[test]
    fn source_session_exposes_proxy_virtual_stream_not_filmstrip() {
        let mut session = frozen_session(EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 25.0,
            duration_frames: 125,
            duration_sec: 5.0,
            segments: Vec::new(),
        });
        session.source_clip = Some(SourceClipPlayback {
            clip_id: "clip_a".into(),
            in_frame: 25,
            out_frame: 150,
            in_sec: 1.0,
            out_sec: 6.0,
            fps: 25.0,
            has_audio: true,
            audio_channels: 2,
        });
        session.clock.virtual_frame = 50;
        session.clock.virtual_sec = 2.0;

        let state = state_from_session(&session);

        assert_eq!(state.timebase_fps, 25.0);
        assert_eq!(state.active.clip_id, "clip_a");
        assert!(state
            .active
            .stream_url
            .contains("/api/story/virtual-stream"));
        assert!(state.active.stream_url.contains("clip_id=clip_a"));
        assert!(state.active.stream_url.contains("in_frame=25"));
        assert!(state.active.stream_url.contains("out_frame=150"));
        assert!(!state.active.stream_url.contains("/filmstrip"));
        assert!(state.active.has_video);
        assert!(state.active.has_audio);
        assert_eq!(state.active.audio_channels, 2);
    }

    #[test]
    fn source_session_exposes_video_only_without_audio_clock() {
        let mut session = frozen_session(EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 25.0,
            duration_frames: 125,
            duration_sec: 5.0,
            segments: Vec::new(),
        });
        session.source_clip = Some(SourceClipPlayback {
            clip_id: "clip_a".into(),
            in_frame: 25,
            out_frame: 150,
            in_sec: 1.0,
            out_sec: 6.0,
            fps: 25.0,
            has_audio: false,
            audio_channels: 0,
        });
        session.clock.virtual_frame = 50;
        session.clock.virtual_sec = 2.0;

        let state = state_from_session(&session);

        assert!(state.active.has_video);
        assert!(!state.active.has_audio);
        assert_eq!(state.active.audio_channels, 0);
        assert_eq!(state.timebase_fps, 25.0);
    }

    #[test]
    fn source_session_can_start_from_virtual_shot_db_truth() {
        let project_id = "story_playback_virtual_truth";
        let base = std::env::temp_dir().join(format!(
            "qnc_story_playback_virtual_truth_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let paths = test_paths(&base);
        let proxy_dir = paths.projects_root.join(project_id).join("proxy");
        std::fs::create_dir_all(&proxy_dir).unwrap();
        let proxy_path = proxy_dir.join("clip_a.mp4");
        std::fs::write(&proxy_path, b"proxy").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        crate::virtual_shots::db::ensure(&paths, project_id, &conn).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, import_status, status, proxy_path,
                 file_extension, fps, has_audio, audio_channels, virtual_name)
             VALUES ('local', 'clip_a', 'Clip A', 'clip_a', 10.0, 'imported', 'ready', ?1, 'mp4',
                     50.0, 1, 2, 'Clip A')",
            rusqlite::params![proxy_path.to_string_lossy().to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO virtual_shots
                (shot_id, clip_id, kind, duration_seconds, in_seconds, out_seconds,
                 fps, source_fps, in_frame, out_frame, duration_frames, quality, virtual_name)
             VALUES
                ('clip_a_root', 'clip_a', 'import_root', 10.0, 1.0, 3.0,
                 50.0, 50.0, 50, 150, 100, 'ready', 'clip_a_root.mp4')",
            [],
        )
        .unwrap();

        let store = PlaybackStore::default();
        let state = store
            .start_virtual_shot(&paths, project_id, "clip_a_root")
            .unwrap();

        assert_eq!(state.timebase_fps, 50.0);
        assert_eq!(state.virtual_frame, 50);
        assert_eq!(state.virtual_sec, 1.0);
        assert_eq!(state.active.clip_id, "clip_a");
        assert_eq!(state.active.source_frame, 50);
        assert_eq!(state.active.source_sec, 1.0);
        assert!(state.active.stream_url.contains("clip_id=clip_a"));
        assert!(state.active.stream_url.contains("in_frame=50"));
        assert!(state.active.stream_url.contains("out_frame=150"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn find_cover_prefers_cover_window() {
        let p = playlist_one();
        let cover_range = &p.segments[0].covers[0];
        let inside_frame =
            cover_range.timeline_start_frame + (cover_range.slot_duration_frames / 2).max(1);
        let (seg, _) = find_segment_frame(&p, inside_frame);
        assert!(seg.is_some());
        let cover = find_cover_frame(&seg.unwrap().covers, inside_frame);
        assert!(cover.is_some());
        assert_eq!(cover.unwrap().cover_id, "cover_a");
    }

    #[test]
    fn find_cover_none_outside_window() {
        let p = playlist_one();
        let before_cover_frame = p.segments[0].covers[0]
            .timeline_start_frame
            .saturating_sub(1);
        let (seg, _) = find_segment_frame(&p, before_cover_frame);
        assert!(seg.is_some());
        assert!(find_cover_frame(&seg.unwrap().covers, before_cover_frame).is_none());
    }

    #[test]
    fn off_segment_outside_cover_is_audio_only_blank_video() {
        let mut playlist = playlist_one();
        playlist.segments[0].kind = "offovi".into();
        let mut session = frozen_session(playlist);
        session.part_streams.insert(
            "part_a".into(),
            PartStreamRef {
                mux_stream_url: String::new(),
                a1_stream_url: "/api/story/virtual-stream?project_id=p&part_id=part_a&audio_only=1"
                    .into(),
                has_audio: true,
                audio_channels: 2,
            },
        );
        session.clock.virtual_frame = 25;
        session.clock.virtual_sec = 1.0;
        let state = state_from_session(&session);
        assert_eq!(state.active.layer, ActiveLayerKind::Part);
        assert!(state.active.video_blank);
        assert!(!state.active.has_video);
        assert!(state.active.has_audio);
        assert_eq!(state.active.audio_channels, 2);
        assert!(state.active.stream_url.is_empty());
        assert!(state.active.a1_stream_url.contains("audio_only=1"));
        assert!(state
            .active
            .mixed_audio_url
            .contains("/api/story/playback/audio"));
        assert!(state
            .active
            .preview_frame_url
            .contains("/api/story/playback/frame"));
    }

    #[test]
    fn frozen_state_uses_session_urls_without_db() {
        let mut session = frozen_session(playlist_one());
        session.clock.virtual_frame = 75;
        session.clock.virtual_sec = 3.0;
        let state = state_from_session(&session);
        assert_eq!(state.active.layer, ActiveLayerKind::Cover);
        assert!(state.active.stream_url.contains("cover_id=cover_a"));
    }

    #[test]
    fn cover_active_layer_uses_cover_source_fps() {
        let mut session = frozen_session(playlist_one());
        let cover = &mut session.playlist.segments[0].covers[0];
        cover.source_in_frame = 100;
        cover.source_out_frame = 200;
        cover.source_fps = 50.0;
        if let Some(stream) = session.cover_streams.get_mut("cover_a") {
            stream.in_frame = 100;
            stream.out_frame = 200;
            stream.fps = 50.0;
        }
        session.clock.virtual_frame = 51;
        session.clock.virtual_sec = frame_to_seconds(51, session.playlist.timeline_fps);

        let state = state_from_session(&session);

        assert_eq!(state.active.layer, ActiveLayerKind::Cover);
        assert_eq!(state.active.source_frame, 1);
        assert!((state.active.source_sec - 0.02).abs() < EPS);
    }

    #[test]
    fn mix_plan_off_window_is_a1_only() {
        use super::super::playback_render::plan_mix_slices;
        let session = frozen_session(playlist_one());
        let slices = plan_mix_slices(&session, 0.5, 1.0);
        assert_eq!(slices.len(), 1);
        assert!(slices[0].cover_id.is_none());
    }

    #[test]
    fn mix_plan_cover_window_includes_cover() {
        use super::super::playback_render::plan_mix_slices;
        let session = frozen_session(playlist_one());
        let slices = plan_mix_slices(&session, 2.5, 1.0);
        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0].cover_id.as_deref(), Some("cover_a"));
    }

    #[test]
    fn mix_plan_cover_seek_uses_cover_source_fps() {
        use super::super::playback_render::plan_mix_slices;
        let mut session = frozen_session(playlist_one());
        session.playlist.segments[0].covers[0].source_fps = 50.0;
        let slices = plan_mix_slices(&session, 2.04, 0.5);

        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0].cover_id.as_deref(), Some("cover_a"));
        assert!((slices[0].cover_source_in_sec - 0.02).abs() < EPS);
    }

    #[test]
    fn mix_plan_splits_at_cover_boundary() {
        use super::super::playback_render::plan_mix_slices;
        let session = frozen_session(playlist_one());
        let slices = plan_mix_slices(&session, 1.0, 3.0);
        assert_eq!(slices.len(), 2);
        assert!(slices[0].cover_id.is_none());
        assert!(slices[1].cover_id.is_some());
    }
}
