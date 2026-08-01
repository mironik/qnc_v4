use std::fs::{File, create_dir_all};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(feature = "audio-device")]
use std::num::NonZero;

use qnc_broadcast_player::{
    AudioFramePacket, AudioOutputAdapter, BroadcastEngineError, BroadcastEngineErrorKind,
    BroadcastEvent, DecodedVideoFrame, FrameNumber, FramePresenter,
};
use qnc_media_ffmpeg::FfmpegVideoPayload;

#[derive(Clone, Debug, Default)]
pub struct EventFramePresenter;

impl FramePresenter for EventFramePresenter {
    type VideoFrame = FfmpegVideoPayload;

    fn present_frame(
        &mut self,
        frame: DecodedVideoFrame<Self::VideoFrame>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        validate_ffmpeg_frame(&frame)?;
        Ok(vec![BroadcastEvent::FramePresented { frame: frame.frame }])
    }
}

#[derive(Clone, Debug)]
pub struct RawFrameFilePresenter {
    output_dir: PathBuf,
}

impl RawFrameFilePresenter {
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }
}

impl FramePresenter for RawFrameFilePresenter {
    type VideoFrame = FfmpegVideoPayload;

    fn present_frame(
        &mut self,
        frame: DecodedVideoFrame<Self::VideoFrame>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        validate_ffmpeg_frame(&frame)?;
        create_dir_all(&self.output_dir)
            .map_err(|err| output_error(BroadcastEngineErrorKind::VideoPresent, err))?;
        let path = frame_file_path(&self.output_dir, frame.frame);
        let mut file = File::create(path)
            .map_err(|err| output_error(BroadcastEngineErrorKind::VideoPresent, err))?;
        file.write_all(&frame.payload.bytes)
            .map_err(|err| output_error(BroadcastEngineErrorKind::VideoPresent, err))?;
        Ok(vec![BroadcastEvent::FramePresented { frame: frame.frame }])
    }
}

#[derive(Clone, Debug)]
pub enum FfmpegFramePresenter {
    Event(EventFramePresenter),
    RawFile(RawFrameFilePresenter),
}

impl FfmpegFramePresenter {
    pub fn event_only() -> Self {
        Self::Event(EventFramePresenter)
    }

    pub fn raw_file(output_dir: impl Into<PathBuf>) -> Self {
        Self::RawFile(RawFrameFilePresenter::new(output_dir))
    }
}

impl Default for FfmpegFramePresenter {
    fn default() -> Self {
        Self::event_only()
    }
}

impl FramePresenter for FfmpegFramePresenter {
    type VideoFrame = FfmpegVideoPayload;

    fn present_frame(
        &mut self,
        frame: DecodedVideoFrame<Self::VideoFrame>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        match self {
            Self::Event(presenter) => presenter.present_frame(frame),
            Self::RawFile(presenter) => presenter.present_frame(frame),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OutputFrameTelemetry<P> {
    inner: P,
    expected_next_frame: Option<FrameNumber>,
}

impl<P> OutputFrameTelemetry<P> {
    pub fn new(inner: P) -> Self {
        Self {
            inner,
            expected_next_frame: None,
        }
    }
}

impl<P> FramePresenter for OutputFrameTelemetry<P>
where
    P: FramePresenter,
{
    type VideoFrame = P::VideoFrame;

    fn present_frame(
        &mut self,
        frame: DecodedVideoFrame<Self::VideoFrame>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let current_frame = frame.frame;
        let mut events = output_frame_continuity_events(self.expected_next_frame, current_frame);
        events.extend(self.inner.present_frame(frame)?);
        self.expected_next_frame = current_frame.checked_add(1);
        Ok(events)
    }
}

#[derive(Clone, Debug, Default)]
pub struct AvSyncTelemetry {
    state: Arc<Mutex<AvSyncTelemetryState>>,
}

impl AvSyncTelemetry {
    pub fn reset(&self) -> Result<(), BroadcastEngineError> {
        let mut state = self.lock()?;
        state.last_audio_frame = None;
        Ok(())
    }

    fn record_audio_frame(&self, frame: FrameNumber) -> Result<(), BroadcastEngineError> {
        let mut state = self.lock()?;
        state.last_audio_frame = Some(frame);
        Ok(())
    }

    fn video_sync_events(
        &self,
        video_frame: FrameNumber,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let state = self.lock()?;
        let Some(audio_frame) = state.last_audio_frame else {
            return Ok(Vec::new());
        };
        if audio_frame == video_frame {
            return Ok(Vec::new());
        }
        Ok(vec![BroadcastEvent::AVSyncWarning {
            offset_frames: signed_frame_offset(audio_frame, video_frame),
        }])
    }

    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, AvSyncTelemetryState>, BroadcastEngineError> {
        self.state
            .lock()
            .map_err(|_| audio_output_error("A/V sync telemetry lock is poisoned"))
    }
}

#[derive(Clone, Debug, Default)]
struct AvSyncTelemetryState {
    last_audio_frame: Option<FrameNumber>,
}

#[derive(Clone, Debug)]
pub struct AvSyncFramePresenter<P> {
    inner: P,
    telemetry: AvSyncTelemetry,
}

impl<P> AvSyncFramePresenter<P> {
    pub fn new(inner: P, telemetry: AvSyncTelemetry) -> Self {
        Self { inner, telemetry }
    }
}

impl<P> FramePresenter for AvSyncFramePresenter<P>
where
    P: FramePresenter,
{
    type VideoFrame = P::VideoFrame;

    fn present_frame(
        &mut self,
        frame: DecodedVideoFrame<Self::VideoFrame>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let video_frame = frame.frame;
        let mut events = self.inner.present_frame(frame)?;
        events.extend(self.telemetry.video_sync_events(video_frame)?);
        Ok(events)
    }
}

pub trait AudioPacketSink<T> {
    fn accept_audio_packet(
        &mut self,
        packet: &AudioFramePacket<T>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError>;

    fn stop(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        Ok(Vec::new())
    }
}

#[derive(Clone, Debug, Default)]
pub struct NullAudioPacketSink;

impl<T> AudioPacketSink<T> for NullAudioPacketSink {
    fn accept_audio_packet(
        &mut self,
        _packet: &AudioFramePacket<T>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        Ok(Vec::new())
    }
}

#[derive(Clone, Debug)]
pub struct RawAudioFileSink {
    output_dir: PathBuf,
}

impl RawAudioFileSink {
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }
}

impl AudioPacketSink<Vec<u8>> for RawAudioFileSink {
    fn accept_audio_packet(
        &mut self,
        packet: &AudioFramePacket<Vec<u8>>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        create_dir_all(&self.output_dir)
            .map_err(|err| output_error(BroadcastEngineErrorKind::AudioOutput, err))?;
        let path = audio_file_path(&self.output_dir, packet.start_frame);
        let mut file = File::create(path)
            .map_err(|err| output_error(BroadcastEngineErrorKind::AudioOutput, err))?;
        file.write_all(&packet.payload)
            .map_err(|err| output_error(BroadcastEngineErrorKind::AudioOutput, err))?;
        Ok(Vec::new())
    }
}

#[cfg(feature = "audio-device")]
pub struct RodioAudioDeviceSink {
    device_sink: rodio::MixerDeviceSink,
    player: Option<rodio::Player>,
    retired_players: Vec<rodio::Player>,
}

#[cfg(feature = "audio-device")]
impl RodioAudioDeviceSink {
    pub fn open_default() -> Result<Self, BroadcastEngineError> {
        let mut device_sink = rodio::DeviceSinkBuilder::open_default_sink()
            .map_err(|err| audio_output_error(err.to_string()))?;
        device_sink.log_on_drop(false);
        let player = rodio::Player::connect_new(device_sink.mixer());
        Ok(Self {
            device_sink,
            player: Some(player),
            retired_players: Vec::new(),
        })
    }

    fn clear_retired_players(&mut self) {
        self.retired_players.retain(|player| !player.empty());
    }
}

#[cfg(feature = "audio-device")]
impl std::fmt::Debug for RodioAudioDeviceSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RodioAudioDeviceSink")
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "audio-device")]
impl AudioPacketSink<Vec<u8>> for RodioAudioDeviceSink {
    fn accept_audio_packet(
        &mut self,
        packet: &AudioFramePacket<Vec<u8>>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let audio_format = packet.audio_format.as_ref().ok_or_else(|| {
            BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "audio packet missing format metadata",
            )
            .with_source_id(packet.source_id.clone())
            .with_frame(packet.start_frame)
        })?;
        let sample_rate = NonZero::new(audio_format.sample_rate_hz).ok_or_else(|| {
            audio_output_error("audio packet sample rate must be greater than zero")
        })?;
        let decoded_samples = pcm_s16le_samples(&packet.payload)?;
        if !decoded_samples
            .len()
            .is_multiple_of(usize::from(audio_format.channel_count))
        {
            return Err(BroadcastEngineError::new(
                BroadcastEngineErrorKind::AudioOutput,
                "audio packet sample count must align to channel count",
            )
            .with_source_id(packet.source_id.clone())
            .with_frame(packet.start_frame));
        }
        let samples =
            stereo_dual_mono_monitor_samples(&decoded_samples, audio_format.channel_count)?;
        let channel_count = NonZero::new(MONITOR_STEREO_CHANNELS).ok_or_else(|| {
            audio_output_error("monitor audio channel count must be greater than zero")
        })?;

        self.clear_retired_players();
        let player = self
            .player
            .get_or_insert_with(|| rodio::Player::connect_new(self.device_sink.mixer()));
        player.append(rodio::buffer::SamplesBuffer::new(
            channel_count,
            sample_rate,
            samples,
        ));
        Ok(Vec::new())
    }

    fn stop(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        if let Some(player) = self.player.take() {
            player.stop();
            self.retired_players.push(player);
        }
        Ok(Vec::new())
    }
}

#[derive(Debug)]
pub enum FfmpegAudioSink {
    None(NullAudioPacketSink),
    RawFile(RawAudioFileSink),
    #[cfg(feature = "audio-device")]
    Device(RodioAudioDeviceSink),
}

impl FfmpegAudioSink {
    pub fn none() -> Self {
        Self::None(NullAudioPacketSink)
    }

    pub fn raw_file(output_dir: impl Into<PathBuf>) -> Self {
        Self::RawFile(RawAudioFileSink::new(output_dir))
    }

    #[cfg(feature = "audio-device")]
    pub fn audio_device() -> Result<Self, BroadcastEngineError> {
        Ok(Self::Device(RodioAudioDeviceSink::open_default()?))
    }
}

impl Default for FfmpegAudioSink {
    fn default() -> Self {
        Self::none()
    }
}

impl AudioPacketSink<Vec<u8>> for FfmpegAudioSink {
    fn accept_audio_packet(
        &mut self,
        packet: &AudioFramePacket<Vec<u8>>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        match self {
            Self::None(sink) => sink.accept_audio_packet(packet),
            Self::RawFile(sink) => sink.accept_audio_packet(packet),
            #[cfg(feature = "audio-device")]
            Self::Device(sink) => sink.accept_audio_packet(packet),
        }
    }

    fn stop(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        match self {
            Self::None(sink) => <NullAudioPacketSink as AudioPacketSink<Vec<u8>>>::stop(sink),
            Self::RawFile(sink) => sink.stop(),
            #[cfg(feature = "audio-device")]
            Self::Device(sink) => sink.stop(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AudioPacketTelemetry<S> {
    inner: S,
    expected_next_frame: Option<FrameNumber>,
}

impl<S> AudioPacketTelemetry<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            expected_next_frame: None,
        }
    }
}

impl<S, T> AudioPacketSink<T> for AudioPacketTelemetry<S>
where
    S: AudioPacketSink<T>,
{
    fn accept_audio_packet(
        &mut self,
        packet: &AudioFramePacket<T>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let mut events =
            audio_packet_continuity_events(self.expected_next_frame, packet.start_frame);
        events.push(BroadcastEvent::BufferStateChanged {
            buffered_frames: packet.frame_count,
        });
        events.extend(self.inner.accept_audio_packet(packet)?);
        self.expected_next_frame = packet.start_frame.checked_add(packet.frame_count);
        Ok(events)
    }

    fn stop(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        self.expected_next_frame = None;
        let mut events = self.inner.stop()?;
        events.push(BroadcastEvent::BufferStateChanged { buffered_frames: 0 });
        Ok(events)
    }
}

#[derive(Clone, Debug)]
pub struct AvSyncAudioPacketSink<S> {
    inner: S,
    telemetry: AvSyncTelemetry,
}

impl<S> AvSyncAudioPacketSink<S> {
    pub fn new(inner: S, telemetry: AvSyncTelemetry) -> Self {
        Self { inner, telemetry }
    }
}

impl<S, T> AudioPacketSink<T> for AvSyncAudioPacketSink<S>
where
    S: AudioPacketSink<T>,
{
    fn accept_audio_packet(
        &mut self,
        packet: &AudioFramePacket<T>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        self.telemetry.record_audio_frame(packet.start_frame)?;
        self.inner.accept_audio_packet(packet)
    }

    fn stop(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        self.telemetry.reset()?;
        self.inner.stop()
    }
}

#[derive(Clone, Debug)]
pub struct AudioOutputWithSink<A, S> {
    inner: A,
    sink: S,
}

impl<A, S> AudioOutputWithSink<A, S> {
    pub fn new(inner: A, sink: S) -> Self {
        Self { inner, sink }
    }
}

impl<A, S> AudioOutputAdapter for AudioOutputWithSink<A, S>
where
    A: AudioOutputAdapter,
    S: AudioPacketSink<A::AudioPacket>,
{
    type AudioPacket = A::AudioPacket;

    fn prepare_audio(
        &mut self,
        source: &qnc_broadcast_player::EngineSourceHandle,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        self.inner.prepare_audio(source)
    }

    fn render_audio_for_frame(
        &mut self,
        request: qnc_broadcast_player::EngineFrameRequest,
    ) -> Result<AudioFramePacket<Self::AudioPacket>, BroadcastEngineError> {
        self.inner.render_audio_for_frame(request)
    }

    fn submit_audio_packet(
        &mut self,
        packet: AudioFramePacket<Self::AudioPacket>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let mut events = self.sink.accept_audio_packet(&packet)?;
        events.extend(self.inner.submit_audio_packet(packet)?);
        Ok(events)
    }

    fn stop_audio(&mut self) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        let mut events = self.sink.stop()?;
        events.extend(self.inner.stop_audio()?);
        Ok(events)
    }
}

fn validate_ffmpeg_frame(
    frame: &DecodedVideoFrame<FfmpegVideoPayload>,
) -> Result<(), BroadcastEngineError> {
    if frame.payload.bytes.is_empty() || frame.payload.frame != frame.frame {
        return Err(BroadcastEngineError::new(
            BroadcastEngineErrorKind::VideoPresent,
            "decoded video payload is invalid",
        )
        .with_source_id(frame.source_id.clone())
        .with_frame(frame.frame));
    }
    Ok(())
}

fn frame_file_path(output_dir: &Path, frame: u64) -> PathBuf {
    output_dir.join(format!("frame-{frame:08}.rgb"))
}

fn audio_file_path(output_dir: &Path, frame: u64) -> PathBuf {
    output_dir.join(format!("audio-frame-{frame:08}.s16le"))
}

fn output_error(kind: BroadcastEngineErrorKind, err: std::io::Error) -> BroadcastEngineError {
    BroadcastEngineError::new(kind, err.to_string())
}

fn output_frame_continuity_events(
    expected_next_frame: Option<FrameNumber>,
    current_frame: FrameNumber,
) -> Vec<BroadcastEvent> {
    let mut events = Vec::new();
    if let Some(expected_frame) = expected_next_frame
        && current_frame > expected_frame
    {
        events.push(BroadcastEvent::DroppedFrame { expected_frame });
    }
    events
}

fn audio_packet_continuity_events(
    expected_next_frame: Option<FrameNumber>,
    current_frame: FrameNumber,
) -> Vec<BroadcastEvent> {
    let mut events = Vec::new();
    if let Some(expected_frame) = expected_next_frame {
        if current_frame > expected_frame {
            events.push(BroadcastEvent::DroppedFrame { expected_frame });
        } else if current_frame < expected_frame {
            events.push(BroadcastEvent::AVSyncWarning {
                offset_frames: signed_frame_offset(current_frame, expected_frame),
            });
        }
    }
    events
}

fn signed_frame_offset(current_frame: FrameNumber, expected_frame: FrameNumber) -> i64 {
    if current_frame >= expected_frame {
        i64::try_from(current_frame - expected_frame).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(expected_frame - current_frame).unwrap_or(i64::MAX)
    }
}

fn audio_output_error(message: impl Into<String>) -> BroadcastEngineError {
    BroadcastEngineError::new(BroadcastEngineErrorKind::AudioOutput, message)
}

#[cfg(feature = "audio-device")]
const MONITOR_STEREO_CHANNELS: u16 = 2;

#[cfg(any(test, feature = "audio-device"))]
fn pcm_s16le_samples(payload: &[u8]) -> Result<Vec<i16>, BroadcastEngineError> {
    if !payload.len().is_multiple_of(2) {
        return Err(audio_output_error(
            "audio packet payload must contain complete s16le samples",
        ));
    }

    Ok(payload
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect())
}

#[cfg(any(test, feature = "audio-device"))]
fn stereo_dual_mono_monitor_samples(
    samples: &[i16],
    source_channels: u16,
) -> Result<Vec<f32>, BroadcastEngineError> {
    if source_channels == 0 {
        return Err(audio_output_error(
            "audio packet channel count must be greater than zero",
        ));
    }
    let source_channels = usize::from(source_channels);
    if !samples.len().is_multiple_of(source_channels) {
        return Err(audio_output_error(
            "audio packet sample count must align to channel count",
        ));
    }

    let mut monitor = Vec::with_capacity(samples.len() / source_channels * 2);
    for frame_samples in samples.chunks_exact(source_channels) {
        let mut left = 0.0_f32;
        let mut right = 0.0_f32;
        for (index, sample) in frame_samples.iter().enumerate() {
            if index % 2 == 0 {
                left += sample_to_unit(*sample);
            } else {
                right += sample_to_unit(*sample);
            }
        }
        monitor.push(left.clamp(-1.0, 1.0));
        monitor.push(right.clamp(-1.0, 1.0));
    }
    Ok(monitor)
}

#[cfg(any(test, feature = "audio-device"))]
fn sample_to_unit(sample: i16) -> f32 {
    if sample == i16::MIN {
        -1.0
    } else {
        f32::from(sample) / f32::from(i16::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_frame_presenter_emits_presented_frame() {
        let mut presenter = EventFramePresenter;

        let events = presenter.present_frame(decoded_frame(7)).unwrap();

        assert!(matches!(
            events.as_slice(),
            [BroadcastEvent::FramePresented { frame: 7 }]
        ));
    }

    #[test]
    fn raw_audio_sink_writes_packet_payload() {
        let dir =
            std::env::temp_dir().join(format!("qnc-player-output-audio-{}", std::process::id()));
        let mut sink = RawAudioFileSink::new(&dir);
        let packet = AudioFramePacket {
            source_id: "src".to_string(),
            start_frame: 12,
            frame_count: 1,
            audio_format: None,
            payload: vec![1, 2, 3, 4],
        };

        sink.accept_audio_packet(&packet).unwrap();

        assert_eq!(
            std::fs::read(audio_file_path(&dir, 12)).unwrap(),
            vec![1, 2, 3, 4]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pcm_s16le_samples_decode_complete_payload() {
        let samples = pcm_s16le_samples(&[0, 0, 255, 127, 0, 128]).unwrap();

        assert_eq!(samples, vec![0, 32767, -32768]);
    }

    #[test]
    fn pcm_s16le_samples_reject_partial_payload() {
        let err = pcm_s16le_samples(&[0]).unwrap_err();

        assert_eq!(err.kind, BroadcastEngineErrorKind::AudioOutput);
    }

    #[test]
    fn stereo_dual_mono_monitor_keeps_single_channel_on_left() {
        let samples = stereo_dual_mono_monitor_samples(&[i16::MAX, 0], 1).unwrap();

        assert_eq!(samples, vec![1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn stereo_dual_mono_monitor_routes_odd_buses_left_even_buses_right() {
        let samples = stereo_dual_mono_monitor_samples(&[1000, 2000, 3000, 4000], 4).unwrap();

        assert_close(samples[0], 4000.0 / f32::from(i16::MAX));
        assert_close(samples[1], 6000.0 / f32::from(i16::MAX));
    }

    #[test]
    fn output_frame_telemetry_reports_frame_gap_before_presenting() {
        let mut presenter = OutputFrameTelemetry::new(EventFramePresenter);
        presenter.present_frame(decoded_frame(7)).unwrap();

        let events = presenter.present_frame(decoded_frame(9)).unwrap();

        assert!(matches!(
            events.as_slice(),
            [
                BroadcastEvent::DroppedFrame { expected_frame: 8 },
                BroadcastEvent::FramePresented { frame: 9 }
            ]
        ));
    }

    #[test]
    fn audio_packet_telemetry_reports_buffer_and_gap() {
        let mut sink = AudioPacketTelemetry::new(NullAudioPacketSink);

        sink.accept_audio_packet(&audio_packet(4, 1)).unwrap();
        let events = sink.accept_audio_packet(&audio_packet(6, 1)).unwrap();

        assert!(
            events
                .iter()
                .any(|event| matches!(event, BroadcastEvent::DroppedFrame { expected_frame: 5 }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            BroadcastEvent::BufferStateChanged { buffered_frames: 1 }
        )));
    }

    #[test]
    fn audio_packet_telemetry_stop_resets_expected_frame() {
        let mut sink = AudioPacketTelemetry::new(NullAudioPacketSink);

        sink.accept_audio_packet(&audio_packet(4, 1)).unwrap();
        let stop_events =
            <AudioPacketTelemetry<NullAudioPacketSink> as AudioPacketSink<Vec<u8>>>::stop(
                &mut sink,
            )
            .unwrap();
        let events = sink.accept_audio_packet(&audio_packet(9, 1)).unwrap();

        assert!(stop_events.iter().any(|event| matches!(
            event,
            BroadcastEvent::BufferStateChanged { buffered_frames: 0 }
        )));
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, BroadcastEvent::DroppedFrame { .. }))
        );
    }

    #[test]
    fn av_sync_telemetry_accepts_matching_audio_and_video_frame() {
        let telemetry = AvSyncTelemetry::default();
        let mut sink = AvSyncAudioPacketSink::new(NullAudioPacketSink, telemetry.clone());
        let mut presenter = AvSyncFramePresenter::new(EventFramePresenter, telemetry);

        sink.accept_audio_packet(&audio_packet(7, 1)).unwrap();
        let events = presenter.present_frame(decoded_frame(7)).unwrap();

        assert!(events.iter().any(|event| {
            matches!(event, BroadcastEvent::FramePresented { frame } if *frame == 7)
        }));
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, BroadcastEvent::AVSyncWarning { .. }))
        );
    }

    #[test]
    fn av_sync_telemetry_reports_audio_video_frame_offset() {
        let telemetry = AvSyncTelemetry::default();
        let mut sink = AvSyncAudioPacketSink::new(NullAudioPacketSink, telemetry.clone());
        let mut presenter = AvSyncFramePresenter::new(EventFramePresenter, telemetry);

        sink.accept_audio_packet(&audio_packet(8, 1)).unwrap();
        let events = presenter.present_frame(decoded_frame(9)).unwrap();

        assert!(events.iter().any(|event| {
            matches!(
                event,
                BroadcastEvent::AVSyncWarning { offset_frames } if *offset_frames == -1
            )
        }));
    }

    #[test]
    fn av_sync_telemetry_stop_clears_stale_audio_frame() {
        let telemetry = AvSyncTelemetry::default();
        let mut sink = AvSyncAudioPacketSink::new(NullAudioPacketSink, telemetry.clone());
        let mut presenter = AvSyncFramePresenter::new(EventFramePresenter, telemetry);

        sink.accept_audio_packet(&audio_packet(3, 1)).unwrap();
        <AvSyncAudioPacketSink<NullAudioPacketSink> as AudioPacketSink<Vec<u8>>>::stop(&mut sink)
            .unwrap();
        let events = presenter.present_frame(decoded_frame(9)).unwrap();

        assert!(
            events
                .iter()
                .all(|event| !matches!(event, BroadcastEvent::AVSyncWarning { .. }))
        );
    }

    fn decoded_frame(frame: u64) -> DecodedVideoFrame<FfmpegVideoPayload> {
        DecodedVideoFrame {
            source_id: "src".to_string(),
            frame,
            video_format: None,
            payload: FfmpegVideoPayload {
                frame,
                bytes: vec![255],
            },
        }
    }

    fn audio_packet(
        start_frame: FrameNumber,
        frame_count: FrameNumber,
    ) -> AudioFramePacket<Vec<u8>> {
        AudioFramePacket {
            source_id: "src".to_string(),
            start_frame,
            frame_count,
            audio_format: None,
            payload: vec![1, 2],
        }
    }

    fn assert_close(left: f32, right: f32) {
        assert!(
            (left - right).abs() <= 0.000_001,
            "left={left} right={right}"
        );
    }
}
