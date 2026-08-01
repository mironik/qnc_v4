//! FFmpeg command backend for native broadcast decode.
//!
//! Consumes `ResolvedFrameDecodePlan`, seeks by `media_seek_sec`, and returns
//! strict `BroadcastVideoPayload` / `BroadcastAudioPayload` values.
//!
//! Live path: per-asset continuous rawvideo pipes for sequential carrier frames
//! (Phase B), with single-frame seek fallback for scrub/still. Hwaccel from
//! [`super::hwaccel`] with software retry on failure.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};

use super::asset::{
    BroadcastMediaAsset, BroadcastMediaLocation, BroadcastResolvedDecodeBackend,
    ResolvedAudioDecodeRequest, ResolvedAudioSource, ResolvedFrameDecodePlan,
    ResolvedVideoDecodeRequest, ResolvedVideoSource,
};
use super::backend::{DecodeError, DecodedAudioBus, DecodedProgramFrame, DecodedVideoLayer};
use super::hwaccel::{ffmpeg_program, player_hwaccel, push_hwaccel_args, DecodeHwaccel};
use super::payload::{
    BroadcastAudioPayload, BroadcastColorSpace, BroadcastPixelFormat, BroadcastScanMode,
    BroadcastVideoPayload, MediaPayloadError,
};
use super::sync::BROADCAST_AUDIO_SAMPLE_RATE_HZ;
use super::timebase::Timebase;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegCommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub expected_stdout_bytes: usize,
}

impl FfmpegCommandSpec {
    fn output_bytes(&self) -> Result<Vec<u8>, DecodeError> {
        let output = Command::new(&self.program)
            .args(&self.args)
            .output()
            .map_err(|err| DecodeError::new(format!("ffmpeg failed to start: {err}")))?;

        if !output.status.success() {
            return Err(DecodeError::new(format!(
                "ffmpeg decode failed ({})",
                output.status
            )));
        }
        if output.stdout.len() != self.expected_stdout_bytes {
            return Err(DecodeError::new(format!(
                "ffmpeg produced {} bytes, expected {}",
                output.stdout.len(),
                self.expected_stdout_bytes
            )));
        }

        Ok(output.stdout)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegBroadcastConfig {
    pub program: String,
    pub output_width: u32,
    pub output_height: u32,
    pub pixel_format: BroadcastPixelFormat,
    pub color_space: BroadcastColorSpace,
    pub scan_mode: BroadcastScanMode,
    pub audio_channels: u8,
    pub hwaccel: DecodeHwaccel,
}

impl Default for FfmpegBroadcastConfig {
    fn default() -> Self {
        Self {
            program: ffmpeg_program(),
            // Montage preview raster — smaller RGBA pipe = less stutter on present path.
            // Edit frame identity stays on carrier FrameNumber; playout full-res is separate.
            output_width: 640,
            output_height: 360,
            pixel_format: BroadcastPixelFormat::Rgba8,
            color_space: BroadcastColorSpace::Bt709,
            scan_mode: BroadcastScanMode::Progressive,
            // Each program bus (A1–A4) is independent mono — not stereo pairs.
            audio_channels: 1,
            hwaccel: player_hwaccel(),
        }
    }
}

struct ContinuousVideoStream {
    child: Child,
    stdout: ChildStdout,
    /// Next source frame this pipe will emit (integer — avoids float seek drift).
    next_source_frame: i64,
    frame_bytes: usize,
}

impl Drop for ContinuousVideoStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct ContinuousAudioStream {
    child: Child,
    stdout: ChildStdout,
    next_start_sample: i64,
    channels: u8,
}

impl Drop for ContinuousAudioStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct FfmpegBroadcastBackend {
    config: FfmpegBroadcastConfig,
    /// Continuous pipes keyed by media input path (base + each overlay asset).
    streams: HashMap<String, ContinuousVideoStream>,
    audio_streams: HashMap<String, ContinuousAudioStream>,
    /// When true, prefer continuous pipes for sequential play; still/scrub uses single-frame.
    continuous: bool,
}

impl FfmpegBroadcastBackend {
    pub fn new(config: FfmpegBroadcastConfig) -> Self {
        Self {
            config,
            streams: HashMap::new(),
            audio_streams: HashMap::new(),
            continuous: true,
        }
    }

    pub fn with_continuous(mut self, continuous: bool) -> Self {
        self.continuous = continuous;
        self
    }

    pub fn config(&self) -> &FfmpegBroadcastConfig {
        &self.config
    }

    pub fn clear_streams(&mut self) {
        self.streams.clear();
        self.audio_streams.clear();
    }

    pub fn set_continuous(&mut self, continuous: bool) {
        if !continuous {
            self.clear_streams();
        }
        self.continuous = continuous;
    }

    pub fn video_command(
        &self,
        request: &ResolvedVideoDecodeRequest,
    ) -> Result<Option<FfmpegCommandSpec>, DecodeError> {
        self.video_command_with_hwaccel(request, self.config.hwaccel)
    }

    fn video_command_with_hwaccel(
        &self,
        request: &ResolvedVideoDecodeRequest,
        hwaccel: DecodeHwaccel,
    ) -> Result<Option<FfmpegCommandSpec>, DecodeError> {
        let ResolvedVideoSource::Media(asset) = &request.resolved_source else {
            return Ok(None);
        };

        let expected_stdout_bytes = self.video_buffer_len()?;
        let input = media_input(asset);
        let pix_fmt = ffmpeg_pix_fmt(self.config.pixel_format);
        let vf = format!(
            "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,format={pix_fmt}",
            self.config.output_width,
            self.config.output_height,
            self.config.output_width,
            self.config.output_height
        );

        let mut args = Vec::new();
        args.push("-hide_banner".into());
        args.push("-loglevel".into());
        args.push("error".into());
        args.push("-nostdin".into());
        push_hwaccel_args(&mut args, hwaccel);
        args.push("-ss".into());
        args.push(format_seconds(request.request.media_seek_sec));
        args.push("-i".into());
        args.push(input);
        args.push("-an".into());
        args.push("-frames:v".into());
        args.push("1".into());
        args.push("-vf".into());
        args.push(vf);
        args.push("-f".into());
        args.push("rawvideo".into());
        args.push("-pix_fmt".into());
        args.push(pix_fmt.into());
        args.push("pipe:1".into());

        Ok(Some(FfmpegCommandSpec {
            program: self.config.program.clone(),
            args,
            expected_stdout_bytes,
        }))
    }

    pub fn audio_command(
        &self,
        request: &ResolvedAudioDecodeRequest,
    ) -> Result<Option<FfmpegCommandSpec>, DecodeError> {
        let ResolvedAudioSource::Media(asset) = &request.resolved_source else {
            return Ok(None);
        };

        // Each program bus is mono PCM after channel extract (never stereo downmix).
        let out_channels = 1_u8;
        let expected_stdout_bytes = request
            .request
            .sample_span
            .len()
            .checked_mul(out_channels as usize)
            .and_then(|samples| samples.checked_mul(std::mem::size_of::<f32>()))
            .ok_or_else(|| DecodeError::new("audio output size overflow"))?;
        let input = media_input(asset);
        let source_ch = source_channel_index(request);

        let mut args = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-nostdin".into(),
            "-ss".into(),
            format_seconds(request.request.media_seek_sec),
            "-i".into(),
            input,
            "-vn".into(),
            "-t".into(),
            format_seconds(
                request.request.sample_span.len() as f64 / BROADCAST_AUDIO_SAMPLE_RATE_HZ as f64,
            ),
        ];
        push_audio_channel_extract(&mut args, asset, source_ch);
        args.extend([
            "-f".into(),
            "f32le".into(),
            "-acodec".into(),
            "pcm_f32le".into(),
            "-ar".into(),
            BROADCAST_AUDIO_SAMPLE_RATE_HZ.to_string(),
            "-ac".into(),
            out_channels.to_string(),
            "pipe:1".into(),
        ]);

        Ok(Some(FfmpegCommandSpec {
            program: self.config.program.clone(),
            args,
            expected_stdout_bytes,
        }))
    }

    fn decode_video(
        &mut self,
        request: &ResolvedVideoDecodeRequest,
        timebase: Timebase,
    ) -> Result<DecodedVideoLayer<BroadcastVideoPayload>, DecodeError> {
        let payload = match &request.resolved_source {
            ResolvedVideoSource::Blank => Some(self.blank_video_payload()?),
            ResolvedVideoSource::Media(asset) => {
                let bytes = if self.continuous {
                    self.decode_video_continuous(asset, request, timebase)?
                } else {
                    self.decode_video_still(request)?
                };
                Some(self.video_payload_from_bytes(bytes)?)
            }
        };

        Ok(DecodedVideoLayer {
            layer_id: request.request.layer_id.clone(),
            role: request.request.role,
            source_frame: request.request.source_frame,
            pts_sec: request.request.pts_sec,
            media_seek_sec: request.request.media_seek_sec,
            payload,
        })
    }

    fn decode_video_still(
        &self,
        request: &ResolvedVideoDecodeRequest,
    ) -> Result<Vec<u8>, DecodeError> {
        let command = self
            .video_command_with_hwaccel(request, self.config.hwaccel)?
            .expect("media video request must build command");
        match command.output_bytes() {
            Ok(bytes) => Ok(bytes),
            Err(err) if self.config.hwaccel != DecodeHwaccel::None => {
                let soft = self
                    .video_command_with_hwaccel(request, DecodeHwaccel::None)?
                    .expect("software video command");
                soft.output_bytes().map_err(|_| err)
            }
            Err(err) => Err(err),
        }
    }

    fn decode_video_continuous(
        &mut self,
        asset: &BroadcastMediaAsset,
        request: &ResolvedVideoDecodeRequest,
        timebase: Timebase,
    ) -> Result<Vec<u8>, DecodeError> {
        let key = media_input(asset);
        let frame = request.request.source_frame.0;
        let seek = request.request.media_seek_sec;

        let reuse = self
            .streams
            .get(&key)
            .is_some_and(|stream| stream.next_source_frame == frame);

        if !reuse {
            self.streams.remove(&key);
            let stream = self.spawn_continuous_stream(asset, seek, frame, timebase)?;
            self.streams.insert(key.clone(), stream);
        }

        let stream = self
            .streams
            .get_mut(&key)
            .ok_or_else(|| DecodeError::new("continuous video stream missing"))?;
        let mut buf = vec![0_u8; stream.frame_bytes];
        match read_exact(&mut stream.stdout, &mut buf) {
            Ok(()) => {
                stream.next_source_frame = frame + 1;
                Ok(buf)
            }
            Err(err) => {
                // Do not fall back to per-frame still during continuous play —
                // that restarts ffmpeg every frame and destroys A/V continuity.
                drop(self.streams.remove(&key));
                Err(err)
            }
        }
    }

    fn spawn_continuous_stream(
        &self,
        asset: &BroadcastMediaAsset,
        seek_sec: f64,
        start_frame: i64,
        timebase: Timebase,
    ) -> Result<ContinuousVideoStream, DecodeError> {
        let frame_bytes = self.video_buffer_len()?;
        let input = media_input(asset);
        let pix_fmt = ffmpeg_pix_fmt(self.config.pixel_format);
        let fps = format!("{}/{}", timebase.num, timebase.den.max(1));
        let vf = format!(
            "fps={fps},scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,format={pix_fmt}",
            self.config.output_width,
            self.config.output_height,
            self.config.output_width,
            self.config.output_height
        );

        let try_spawn = |hwaccel: DecodeHwaccel| -> Result<ContinuousVideoStream, DecodeError> {
            let mut args = Vec::new();
            args.push("-hide_banner".into());
            args.push("-loglevel".into());
            args.push("error".into());
            args.push("-nostdin".into());
            push_hwaccel_args(&mut args, hwaccel);
            args.push("-ss".into());
            args.push(format_seconds(seek_sec));
            args.push("-i".into());
            args.push(input.clone());
            args.push("-an".into());
            args.push("-vf".into());
            args.push(vf.clone());
            args.push("-f".into());
            args.push("rawvideo".into());
            args.push("-pix_fmt".into());
            args.push(pix_fmt.into());
            args.push("pipe:1".into());

            let mut child = Command::new(&self.config.program)
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|err| DecodeError::new(format!("ffmpeg continuous start: {err}")))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| DecodeError::new("ffmpeg continuous missing stdout"))?;
            Ok(ContinuousVideoStream {
                child,
                stdout,
                next_source_frame: start_frame,
                frame_bytes,
            })
        };

        match try_spawn(self.config.hwaccel) {
            Ok(stream) => Ok(stream),
            Err(err) if self.config.hwaccel != DecodeHwaccel::None => {
                try_spawn(DecodeHwaccel::None).map_err(|_| err)
            }
            Err(err) => Err(err),
        }
    }

    fn decode_audio(
        &mut self,
        request: &ResolvedAudioDecodeRequest,
    ) -> Result<DecodedAudioBus<BroadcastAudioPayload>, DecodeError> {
        let payload = match &request.resolved_source {
            ResolvedAudioSource::Silence => Some(self.silence_audio_payload(request)?),
            ResolvedAudioSource::Media(asset) => {
                let bytes = if self.continuous {
                    self.decode_audio_continuous(asset, request)?
                } else {
                    self.decode_audio_still(request)?
                };
                Some(self.audio_payload_from_bytes(request, bytes)?)
            }
        };

        Ok(DecodedAudioBus {
            layer_id: request.request.layer_id.clone(),
            channel: request.request.channel,
            mix: request.request.mix,
            source_frame: request.request.source_frame,
            pts_sec: request.request.pts_sec,
            media_seek_sec: request.request.media_seek_sec,
            sample_rate_hz: request.request.sample_rate_hz,
            sample_span: request.request.sample_span,
            payload,
        })
    }

    fn decode_audio_still(
        &self,
        request: &ResolvedAudioDecodeRequest,
    ) -> Result<Vec<u8>, DecodeError> {
        let command = self
            .audio_command(request)?
            .expect("media audio request must build command");
        command.output_bytes()
    }

    fn decode_audio_continuous(
        &mut self,
        asset: &BroadcastMediaAsset,
        request: &ResolvedAudioDecodeRequest,
    ) -> Result<Vec<u8>, DecodeError> {
        // Per bus/channel pipe so A1+A2 in the same frame do not steal PCM.
        let key = format!("{}#{}", media_input(asset), request.request.channel.get());
        let start = request.request.sample_span.start_sample;
        let end = request.request.sample_span.end_exclusive;
        let channels = 1_usize;
        let byte_len = request
            .request
            .sample_span
            .len()
            .checked_mul(channels)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| DecodeError::new("audio continuous size overflow"))?;

        let needs_restart = match self.audio_streams.get(&key) {
            None => true,
            // Backward seek → new pipe. Forward gaps are skipped in-stream.
            Some(stream) => stream.next_start_sample > start,
        };

        if needs_restart {
            self.audio_streams.remove(&key);
            let stream = self.spawn_continuous_audio(asset, request)?;
            self.audio_streams.insert(key.clone(), stream);
        }

        // Skip ahead inside the pipe when decode jumped forward by a few frames.
        {
            let stream = self
                .audio_streams
                .get_mut(&key)
                .ok_or_else(|| DecodeError::new("continuous audio stream missing"))?;
            while stream.next_start_sample < start {
                let gap = (start - stream.next_start_sample) as usize;
                let skip_bytes = gap
                    .checked_mul(channels)
                    .and_then(|n| n.checked_mul(4))
                    .ok_or_else(|| DecodeError::new("audio skip overflow"))?;
                let chunk = skip_bytes.min(64 * 1024);
                let mut discard = vec![0_u8; chunk];
                read_exact(&mut stream.stdout, &mut discard)?;
                let skipped_samples = (chunk / (channels * 4)) as i64;
                stream.next_start_sample += skipped_samples;
            }
        }

        let stream = self
            .audio_streams
            .get_mut(&key)
            .ok_or_else(|| DecodeError::new("continuous audio stream missing"))?;
        if stream.next_start_sample != start {
            return Err(DecodeError::new(format!(
                "audio continuous desync: pipe {} want {}",
                stream.next_start_sample, start
            )));
        }
        let mut buf = vec![0_u8; byte_len];
        match read_exact(&mut stream.stdout, &mut buf) {
            Ok(()) => {
                stream.next_start_sample = end;
                Ok(buf)
            }
            Err(err) => {
                // No still-fallback: that makes speech unintelligible.
                drop(self.audio_streams.remove(&key));
                Err(err)
            }
        }
    }

    fn spawn_continuous_audio(
        &self,
        asset: &BroadcastMediaAsset,
        request: &ResolvedAudioDecodeRequest,
    ) -> Result<ContinuousAudioStream, DecodeError> {
        let channels = 1_u8;
        let source_ch = source_channel_index(request);
        let input = media_input(asset);
        let mut args = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-nostdin".into(),
            "-ss".into(),
            format_seconds(request.request.media_seek_sec),
            "-i".into(),
            input,
            "-vn".into(),
        ];
        push_audio_channel_extract(&mut args, asset, source_ch);
        args.extend([
            "-f".into(),
            "f32le".into(),
            "-acodec".into(),
            "pcm_f32le".into(),
            "-ar".into(),
            BROADCAST_AUDIO_SAMPLE_RATE_HZ.to_string(),
            "-ac".into(),
            channels.to_string(),
            "pipe:1".into(),
        ]);
        let mut child = Command::new(&self.config.program)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| DecodeError::new(format!("ffmpeg continuous audio start: {err}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DecodeError::new("ffmpeg continuous audio missing stdout"))?;
        Ok(ContinuousAudioStream {
            child,
            stdout,
            next_start_sample: request.request.sample_span.start_sample,
            channels,
        })
    }

    fn video_buffer_len(&self) -> Result<usize, DecodeError> {
        (self.config.output_width as usize)
            .checked_mul(self.config.output_height as usize)
            .and_then(|pixels| pixels.checked_mul(self.config.pixel_format.bytes_per_pixel()))
            .ok_or_else(|| DecodeError::new("video output size overflow"))
    }

    fn blank_video_payload(&self) -> Result<BroadcastVideoPayload, DecodeError> {
        self.video_payload_from_bytes(vec![0; self.video_buffer_len()?])
    }

    fn video_payload_from_bytes(
        &self,
        bytes: Vec<u8>,
    ) -> Result<BroadcastVideoPayload, DecodeError> {
        BroadcastVideoPayload::with_layout(
            self.config.output_width,
            self.config.output_height,
            self.config.output_width as usize * self.config.pixel_format.bytes_per_pixel(),
            self.config.pixel_format,
            self.config.color_space,
            self.config.scan_mode,
            bytes,
        )
        .map_err(payload_error)
    }

    fn silence_audio_payload(
        &self,
        request: &ResolvedAudioDecodeRequest,
    ) -> Result<BroadcastAudioPayload, DecodeError> {
        BroadcastAudioPayload::silence_for_request(&request.request, 1).map_err(payload_error)
    }

    fn audio_payload_from_bytes(
        &self,
        request: &ResolvedAudioDecodeRequest,
        bytes: Vec<u8>,
    ) -> Result<BroadcastAudioPayload, DecodeError> {
        let samples = bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect::<Vec<_>>();
        BroadcastAudioPayload::new_f32_interleaved(
            request.request.sample_rate_hz,
            1,
            request.request.sample_span,
            samples,
        )
        .map_err(payload_error)
    }
}

impl BroadcastResolvedDecodeBackend for FfmpegBroadcastBackend {
    type VideoPayload = BroadcastVideoPayload;
    type AudioPayload = BroadcastAudioPayload;

    fn decode_resolved_frame(
        &mut self,
        plan: &ResolvedFrameDecodePlan,
    ) -> Result<DecodedProgramFrame<Self::VideoPayload, Self::AudioPayload>, DecodeError> {
        let timebase = plan
            .video
            .iter()
            .find_map(|v| match &v.resolved_source {
                ResolvedVideoSource::Media(asset) => Some(asset.source_timebase),
                _ => None,
            })
            .or_else(|| {
                plan.audio.iter().find_map(|a| match &a.resolved_source {
                    ResolvedAudioSource::Media(asset) => Some(asset.source_timebase),
                    _ => None,
                })
            })
            .unwrap_or_else(|| Timebase::from_source_fps(25.0));

        let mut frame = DecodedProgramFrame {
            source_frame: plan.source_frame,
            pts_sec: plan.pts_sec,
            video: Vec::with_capacity(plan.video.len()),
            audio: Vec::with_capacity(plan.audio.len()),
            markers: plan.markers.clone(),
            effects: plan.effects.clone(),
        };

        for request in &plan.video {
            frame.video.push(self.decode_video(request, timebase)?);
        }
        for request in &plan.audio {
            frame.audio.push(self.decode_audio(request)?);
        }

        frame.validate_against_plan(&plan.unresolved_plan())?;
        frame.validate_payload_contract().map_err(payload_error)?;
        Ok(frame)
    }
}

fn read_exact(stdout: &mut ChildStdout, buf: &mut [u8]) -> Result<(), DecodeError> {
    let mut filled = 0;
    while filled < buf.len() {
        match stdout.read(&mut buf[filled..]) {
            Ok(0) => return Err(DecodeError::new("ffmpeg continuous EOF")),
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(DecodeError::new(format!("ffmpeg continuous read: {err}"))),
        }
    }
    Ok(())
}

fn media_input(asset: &BroadcastMediaAsset) -> String {
    match &asset.location {
        BroadcastMediaLocation::LocalPath(path) => path.to_string_lossy().into_owned(),
        BroadcastMediaLocation::Url(url) => url.clone(),
    }
}

/// Program bus An → source channel index n-1 (A1→0, A2→1, …).
fn source_channel_index(request: &ResolvedAudioDecodeRequest) -> u8 {
    request.request.channel.get().saturating_sub(1)
}

/// Extract one source channel as mono — no stereo→mono downmix onto the bus.
fn push_audio_channel_extract(
    args: &mut Vec<String>,
    asset: &BroadcastMediaAsset,
    channel_idx: u8,
) {
    if asset.uses_discrete_mono_streams() {
        // MXF-style: one mono stream per track.
        args.push("-map".into());
        args.push(format!("0:a:{channel_idx}"));
    } else {
        // Interleaved stereo/multi in one stream: take only cN.
        args.push("-af".into());
        args.push(format!("pan=mono|c0=c{channel_idx}"));
    }
}

fn ffmpeg_pix_fmt(pixel_format: BroadcastPixelFormat) -> &'static str {
    match pixel_format {
        BroadcastPixelFormat::Rgba8 => "rgba",
        BroadcastPixelFormat::Bgra8 => "bgra",
    }
}

fn format_seconds(seconds: f64) -> String {
    format!("{:.6}", seconds.max(0.0))
}

fn payload_error(err: MediaPayloadError) -> DecodeError {
    DecodeError::new(err.message)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::broadcast::asset::{
        BroadcastMediaAsset, InMemoryMediaResolver, ResolvedFrameDecodePlan,
    };
    use crate::broadcast::timebase::{FrameNumber, FrameRange, Timebase};
    use crate::broadcast::{
        BroadcastFrameScheduler, BroadcastPlaybackSource, BroadcastProgramGraph,
        BroadcastRenderPlan,
    };

    fn source_plan(source_fps: f64) -> ResolvedFrameDecodePlan {
        let source_timebase = Timebase::from_source_fps(source_fps);
        let source = BroadcastPlaybackSource {
            project_id: "project".into(),
            virtual_shot_id: "shot".into(),
            clip_id: "clip".into(),
            source_range: FrameRange::new(FrameNumber(100), FrameNumber(200)),
            source_timebase,
            has_video: true,
            has_audio: true,
            audio_channels: 2,
        };
        let graph = BroadcastProgramGraph::from_source_virtual_shot(&source);
        let render_plan = BroadcastRenderPlan::from_graph(&graph);
        let scheduler = BroadcastFrameScheduler::new(render_plan.clone());
        let plan = crate::broadcast::FrameDecodePlan::from_scheduled(
            &render_plan,
            scheduler.schedule_frame(FrameNumber(125)),
        );
        let resolver = InMemoryMediaResolver::new().with_asset(BroadcastMediaAsset::proxy_local(
            "project",
            "shot",
            "clip",
            PathBuf::from("media/proxy.mxf"),
            source_timebase,
            true,
            true,
        ));
        ResolvedFrameDecodePlan::resolve(&plan, &resolver).unwrap()
    }

    #[test]
    fn video_command_seeks_to_media_time_not_program_pts() {
        let backend = FfmpegBroadcastBackend::new(FfmpegBroadcastConfig {
            output_width: 2,
            output_height: 2,
            hwaccel: DecodeHwaccel::None,
            ..Default::default()
        });
        let plan = source_plan(50.0);
        let expected_media_seek =
            Timebase::from_source_fps(50.0).seconds_at_frame(FrameNumber(125));

        let command = backend.video_command(&plan.video[0]).unwrap().unwrap();

        assert_eq!(plan.pts_sec, 0.5);
        assert_eq!(plan.video[0].request.media_seek_sec, expected_media_seek);
        assert!(command
            .args
            .iter()
            .any(|a| a == &format!("{expected_media_seek:.6}")));
        assert_eq!(command.expected_stdout_bytes, 16);
        assert!(command.args.iter().any(|arg| arg.contains("format=rgba")));
    }

    #[test]
    fn audio_command_uses_request_sample_span_duration() {
        let backend = FfmpegBroadcastBackend::new(FfmpegBroadcastConfig {
            audio_channels: 1,
            hwaccel: DecodeHwaccel::None,
            ..Default::default()
        });
        let plan = source_plan(50.0);
        let expected_media_seek =
            Timebase::from_source_fps(50.0).seconds_at_frame(FrameNumber(125));

        let command = backend.audio_command(&plan.audio[0]).unwrap().unwrap();

        assert!(command
            .args
            .iter()
            .any(|a| a == &format!("{expected_media_seek:.6}")));
        assert!(command.args.iter().any(|arg| arg == "0.020000"));
        assert_eq!(command.expected_stdout_bytes, 960 * 4);
        assert!(
            command.args.iter().any(|arg| arg == "pan=mono|c0=c0"),
            "A1 must extract source ch0 without downmix: {:?}",
            command.args
        );
    }

    #[test]
    fn audio_command_extracts_a2_as_source_channel_one() {
        let backend = FfmpegBroadcastBackend::new(FfmpegBroadcastConfig {
            hwaccel: DecodeHwaccel::None,
            ..Default::default()
        });
        let plan = source_plan(50.0);
        assert!(plan.audio.len() >= 2, "stereo source rack has A1+A2 media");
        let command = backend.audio_command(&plan.audio[1]).unwrap().unwrap();
        assert!(
            command.args.iter().any(|arg| arg == "pan=mono|c0=c1"),
            "A2 must extract source ch1: {:?}",
            command.args
        );
    }

    #[test]
    fn video_command_can_include_hwaccel() {
        let backend = FfmpegBroadcastBackend::new(FfmpegBroadcastConfig {
            output_width: 2,
            output_height: 2,
            hwaccel: DecodeHwaccel::D3d11va,
            ..Default::default()
        });
        let plan = source_plan(25.0);
        let command = backend.video_command(&plan.video[0]).unwrap().unwrap();
        let joined = command.args.join(" ");
        assert!(joined.contains("-hwaccel d3d11va"));
    }
}
