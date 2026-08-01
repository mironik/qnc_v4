use std::collections::BTreeMap;
use std::env;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::process::{Child, Command as ProcessCommand, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use qnc_broadcast_player::{
    AudioFormat, BroadcastEngineError, BroadcastEvent, BroadcastPlaybackRequest,
    BroadcastPlayerProtocolCommand, BroadcastPlayerProtocolEvent, ClockTick, ColorSpace,
    DecodedVideoFrame, FieldMode, FrameClockConfig, FrameClockRate, FramePresenter, FrameRange,
    SourceRuntime, Timebase, TransportEngine, VideoFormat,
};
use qnc_media_ffmpeg::{
    FfmpegAudioDecodeOptions, FfmpegAudioOutput, FfmpegDecodeOptions, FfmpegHardwareDecode,
    FfmpegSourceOpen, FfmpegSourceRegistry, FfmpegToolchain, FfmpegVideoDecode, FfmpegVideoPayload,
    available_hardware_decode_backends_with_toolchain, probe_source_runtime_with_toolchain,
};
use qnc_player_monitor_bridge::{
    MonitorBridgeError, MonitorEventBridge, MonitorFrameMapper, MonitorFramePresenter,
    SharedPlayerMonitor, rgb24_monitor_frame_buffer,
};
use qnc_player_output::{
    AudioOutputWithSink, AudioPacketTelemetry, AvSyncAudioPacketSink, AvSyncFramePresenter,
    AvSyncTelemetry, FfmpegAudioSink, FfmpegFramePresenter, OutputFrameTelemetry,
};
use qnc_player_runtime::{BroadcastPlayerRuntime, PlayerRuntimeCommand};
use serde_json::{Value, json};

const RUNNER_REALTIME_MAX_CATCHUP_FRAMES: usize = 1;
const RUNNER_TOOL_CHECK_TIMEOUT: Duration = Duration::from_millis(5_000);
const RUNNER_TOOL_TERMINATE_TIMEOUT: Duration = Duration::from_millis(5);
const RUNNER_TOOL_CHECK_POLL: Duration = Duration::from_millis(10);

type RunnerRuntime = BroadcastPlayerRuntime<
    TransportEngine<
        FfmpegSourceOpen,
        FfmpegVideoDecode,
        AudioOutputWithSink<
            FfmpegAudioOutput,
            AudioPacketTelemetry<AvSyncAudioPacketSink<FfmpegAudioSink>>,
        >,
        RunnerFramePresenter,
    >,
>;

struct RunnerMonitor {
    shared: SharedPlayerMonitor,
    events: MonitorEventBridge,
}

impl RunnerMonitor {
    fn new() -> Self {
        let shared = SharedPlayerMonitor::default();
        let events = MonitorEventBridge::new(shared.clone());
        Self { shared, events }
    }

    fn shared(&self) -> SharedPlayerMonitor {
        self.shared.clone()
    }

    fn apply_events(&self, events: &[BroadcastPlayerProtocolEvent]) -> Result<(), String> {
        self.events
            .apply_events(events)
            .map_err(|error| error.to_string())
    }

    fn snapshot(&self) -> Result<qnc_player_monitor_bridge::PlayerMonitorState, String> {
        self.shared.snapshot().map_err(|error| error.to_string())
    }
}

enum RunnerFramePresenter {
    Plain(AvSyncFramePresenter<OutputFrameTelemetry<FfmpegFramePresenter>>),
    Monitor(
        MonitorFramePresenter<
            AvSyncFramePresenter<OutputFrameTelemetry<FfmpegFramePresenter>>,
            FfmpegMonitorFrameMapper,
        >,
    ),
}

impl FramePresenter for RunnerFramePresenter {
    type VideoFrame = FfmpegVideoPayload;

    fn present_frame(
        &mut self,
        frame: DecodedVideoFrame<Self::VideoFrame>,
    ) -> Result<Vec<BroadcastEvent>, BroadcastEngineError> {
        match self {
            Self::Plain(presenter) => presenter.present_frame(frame),
            Self::Monitor(presenter) => presenter.present_frame(frame),
        }
    }
}

#[derive(Clone, Debug)]
struct FfmpegMonitorFrameMapper {
    source_id: String,
    video_format: VideoFormat,
}

impl MonitorFrameMapper<FfmpegVideoPayload> for FfmpegMonitorFrameMapper {
    fn map_frame(
        &mut self,
        frame: &DecodedVideoFrame<FfmpegVideoPayload>,
    ) -> Result<qnc_player_monitor_bridge::MonitorFrameBuffer, MonitorBridgeError> {
        let video_format = frame
            .video_format
            .clone()
            .unwrap_or_else(|| self.video_format.clone());
        let source_id = if frame.source_id.is_empty() {
            self.source_id.clone()
        } else {
            frame.source_id.clone()
        };
        rgb24_monitor_frame_buffer(
            source_id,
            frame.frame,
            video_format,
            frame.payload.bytes.clone(),
        )
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = RunnerArgs::parse(env::args().skip(1))?;
    if args.help {
        println!("{}", usage());
        return Ok(());
    }
    let toolchain = runner_toolchain(&args)?;
    if args.list_hwaccels {
        require_tool(toolchain.ffmpeg(), "ffmpeg")?;
        emit_hardware_decode_backends(&toolchain)?;
        return Ok(());
    }

    require_tool(toolchain.ffmpeg(), "ffmpeg")?;
    if args.probe_source_runtime {
        require_tool(toolchain.ffprobe(), "ffprobe")?;
    }

    let source = source_runtime_from_args(&args, &toolchain)?;
    let diagnostic_source = source.clone();
    let monitor_source_id = source.source_id.clone();
    let monitor_video_format = source.video_format.clone();
    let playback_timebase = source.timebase;
    let out_frame = args.out_frame.unwrap_or(source.duration_frames);
    let range = FrameRange::new(args.in_frame, out_frame)?;
    let request = BroadcastPlaybackRequest::new(args.request_id.clone(), source)?
        .with_range(range)?
        .with_rate(args.rate_num, args.rate_den)?;
    let monitor = args.emit_monitor_state.then(RunnerMonitor::new);
    let av_sync = AvSyncTelemetry::default();

    let registry = source_registry(&args);
    let video_presenter = build_video_presenter(
        args.output_dir.as_ref(),
        monitor.as_ref().map(RunnerMonitor::shared),
        monitor_source_id,
        monitor_video_format,
        av_sync.clone(),
    );
    let audio_sink = build_audio_sink(args.output_dir.as_ref(), args.audio_device)?;
    let mut video_decode_options = FfmpegDecodeOptions::software()
        .with_toolchain(toolchain.clone())
        .with_hardware_decode(args.hardware_decode.clone());
    if let Some(video_prefetch_frames) = args.video_prefetch_frames {
        video_decode_options =
            video_decode_options.with_video_prefetch_frames(video_prefetch_frames);
    }
    if let Some(video_cache_frames) = args.video_cache_frames {
        video_decode_options = video_decode_options.with_video_cache_frames(video_cache_frames);
    }
    if let Some(video_cache_bytes) = args.video_cache_bytes {
        video_decode_options = video_decode_options.with_video_cache_bytes(video_cache_bytes);
    }
    if let Some(read_timeout_ms) = args.ffmpeg_read_timeout_ms {
        video_decode_options =
            video_decode_options.with_read_timeout(Duration::from_millis(read_timeout_ms));
    }
    let mut audio_decode_options =
        FfmpegAudioDecodeOptions::default().with_toolchain(toolchain.clone());
    if let Some(audio_prefetch_frames) = args.audio_prefetch_frames {
        audio_decode_options =
            audio_decode_options.with_audio_prefetch_frames(audio_prefetch_frames);
    }
    if let Some(audio_cache_frames) = args.audio_cache_frames {
        audio_decode_options = audio_decode_options.with_audio_cache_frames(audio_cache_frames);
    }
    if let Some(audio_cache_bytes) = args.audio_cache_bytes {
        audio_decode_options = audio_decode_options.with_audio_cache_bytes(audio_cache_bytes);
    }
    if let Some(read_timeout_ms) = args.ffmpeg_read_timeout_ms {
        audio_decode_options =
            audio_decode_options.with_read_timeout(Duration::from_millis(read_timeout_ms));
    }
    let mut runtime = build_runtime(
        registry,
        video_decode_options,
        audio_decode_options,
        audio_sink,
        video_presenter,
        av_sync,
    );

    runtime.dispatch_at(
        PlayerRuntimeCommand::new(
            "runner-set-playback",
            BroadcastPlayerProtocolCommand::SetPlaybackRequest {
                request: Box::new(request),
            },
        ),
        0,
    );
    let events = drain_runtime_events(&mut runtime, monitor.as_ref())?;
    let failed = has_failure_event(&events);
    emit_events("set_playback_request", None, events)?;
    if failed {
        return Err("playback setup failed".to_string());
    }

    if args.cue_latency_diagnostics {
        run_cue_latency_diagnostics(&mut runtime, monitor.as_ref(), &diagnostic_source, &args)?;
        emit_final_state(&runtime, false)?;
        if let Some(monitor) = &monitor {
            emit_monitor_state(monitor)?;
        }
        return Ok(());
    }

    if args.realtime_diagnostics {
        let frame_interval = frame_interval_ticks(playback_timebase, args.rate_num, args.rate_den)?;
        let reached_boundary = run_realtime_diagnostics(
            &mut runtime,
            monitor.as_ref(),
            diagnostic_source,
            frame_interval,
            range,
            &args,
        )?;
        emit_final_state(&runtime, reached_boundary)?;
        if let Some(monitor) = &monitor {
            emit_monitor_state(monitor)?;
        }
        if args.require_boundary && !reached_boundary {
            return Err("diagnostic playback did not reach execution boundary".to_string());
        }
        return Ok(());
    }

    if args.stdin_jsonl {
        return run_stdin_jsonl(runtime, monitor);
    }

    runtime.dispatch_at(
        PlayerRuntimeCommand::new("runner-play", BroadcastPlayerProtocolCommand::Play),
        0,
    );
    let events = drain_runtime_events(&mut runtime, monitor.as_ref())?;
    let failed = has_failure_event(&events);
    emit_events("play", None, events)?;
    if failed {
        return Err("playback start failed".to_string());
    }

    let frame_interval = frame_interval_ticks(playback_timebase, args.rate_num, args.rate_den)?;
    let max_ticks = args.max_ticks.unwrap_or_else(|| {
        range
            .end_frame
            .saturating_sub(range.start_frame)
            .saturating_add(2)
    });
    let reached_boundary = if args.realtime {
        run_realtime_tick_loop(&mut runtime, max_ticks, frame_interval, monitor.as_ref())?
    } else {
        run_synthetic_tick_loop(&mut runtime, max_ticks, frame_interval, monitor.as_ref())?
    };

    emit_final_state(&runtime, reached_boundary)?;
    if let Some(monitor) = &monitor {
        emit_monitor_state(monitor)?;
    }
    if args.require_boundary && !reached_boundary {
        return Err("playback did not reach execution boundary before max tick count".to_string());
    }

    Ok(())
}

fn run_synthetic_tick_loop(
    runtime: &mut RunnerRuntime,
    max_ticks: u64,
    frame_interval: ClockTick,
    monitor: Option<&RunnerMonitor>,
) -> Result<bool, String> {
    let mut now_tick: ClockTick = 0;
    for tick_index in 0..max_ticks {
        runtime.tick(now_tick);
        let events = drain_runtime_events(runtime, monitor)?;
        let reached_boundary = has_boundary_event(&events);
        let failed = has_failure_event(&events);
        emit_events("tick", Some(tick_index), events)?;
        if reached_boundary {
            return Ok(true);
        }
        if failed {
            return Err("playback failed before execution boundary".to_string());
        }
        now_tick = now_tick.saturating_add(frame_interval);
    }

    Ok(false)
}

fn run_realtime_tick_loop(
    runtime: &mut RunnerRuntime,
    max_ticks: u64,
    frame_interval: ClockTick,
    monitor: Option<&RunnerMonitor>,
) -> Result<bool, String> {
    let started_at = Instant::now();
    for tick_index in 0..max_ticks {
        sleep_until_tick(
            started_at,
            frame_interval.saturating_mul(ClockTick::from(tick_index)),
        );
        runtime.tick(started_at.elapsed().as_nanos());
        let events = drain_runtime_events(runtime, monitor)?;
        let reached_boundary = has_boundary_event(&events);
        let failed = has_failure_event(&events);
        emit_events("tick", Some(tick_index), events)?;
        if reached_boundary {
            return Ok(true);
        }
        if failed {
            return Err("playback failed before execution boundary".to_string());
        }
    }

    Ok(false)
}

fn sleep_until_tick(started_at: Instant, target_tick: ClockTick) {
    let target_delay = tick_to_duration(target_tick);
    let elapsed = started_at.elapsed();
    if target_delay > elapsed {
        thread::sleep(target_delay - elapsed);
    }
}

fn tick_to_duration(tick: ClockTick) -> Duration {
    Duration::from_nanos(u64::try_from(tick).unwrap_or(u64::MAX))
}

fn build_runtime(
    registry: FfmpegSourceRegistry,
    video_decode_options: FfmpegDecodeOptions,
    audio_decode_options: FfmpegAudioDecodeOptions,
    audio_sink: FfmpegAudioSink,
    video_presenter: RunnerFramePresenter,
    av_sync: AvSyncTelemetry,
) -> RunnerRuntime {
    let engine = TransportEngine::new(
        FfmpegSourceOpen::new(registry.clone()),
        FfmpegVideoDecode::with_options(registry.clone(), video_decode_options),
        AudioOutputWithSink::new(
            FfmpegAudioOutput::with_options(registry, audio_decode_options),
            AudioPacketTelemetry::new(AvSyncAudioPacketSink::new(audio_sink, av_sync)),
        ),
        video_presenter,
    )
    .with_max_catchup_frames(RUNNER_REALTIME_MAX_CATCHUP_FRAMES);
    BroadcastPlayerRuntime::new(engine)
}

fn build_video_presenter(
    output_dir: Option<&PathBuf>,
    monitor: Option<SharedPlayerMonitor>,
    source_id: String,
    video_format: Option<VideoFormat>,
    av_sync: AvSyncTelemetry,
) -> RunnerFramePresenter {
    let presenter = AvSyncFramePresenter::new(
        OutputFrameTelemetry::new(
            output_dir
                .map(|dir| FfmpegFramePresenter::raw_file(dir.join("video")))
                .unwrap_or_default(),
        ),
        av_sync,
    );
    if let (Some(monitor), Some(video_format)) = (monitor, video_format) {
        return RunnerFramePresenter::Monitor(MonitorFramePresenter::new(
            presenter,
            monitor,
            FfmpegMonitorFrameMapper {
                source_id,
                video_format,
            },
        ));
    }
    RunnerFramePresenter::Plain(presenter)
}

fn run_stdin_jsonl(
    mut runtime: RunnerRuntime,
    monitor: Option<RunnerMonitor>,
) -> Result<(), String> {
    let stdin = io::stdin();
    let mut reached_boundary = false;
    for (line_index, line) in stdin.lock().lines().enumerate() {
        let line = line.map_err(|err| err.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let input = parse_jsonl_input(&line)?;
        if let Some(command) = input.command {
            runtime.dispatch_at(command, input.tick);
            let events = drain_runtime_events(&mut runtime, monitor.as_ref())?;
            reached_boundary |= has_boundary_event(&events);
            emit_events("stdin_command", Some(line_index as u64), events)?;
        } else {
            runtime.tick(input.tick);
            let events = drain_runtime_events(&mut runtime, monitor.as_ref())?;
            reached_boundary |= has_boundary_event(&events);
            emit_events("stdin_tick", Some(line_index as u64), events)?;
        }
    }
    emit_final_state(&runtime, reached_boundary)?;
    if let Some(monitor) = &monitor {
        emit_monitor_state(monitor)?;
    }
    Ok(())
}

#[derive(Debug)]
struct RealtimeDiagnostics {
    source_id: String,
    timebase: Timebase,
    audio_device: bool,
    command_records: Vec<Value>,
    segment_records: Vec<Value>,
    command_dropped_frame_count: u64,
    playback_dropped_frame_count: u64,
    command_av_sync_warning_count: u64,
    playback_av_sync_warning_count: u64,
    playback_error_count: u64,
}

#[derive(Debug)]
struct CueLatencyTotals {
    dropped_frame_count: u64,
    av_sync_warning_count: u64,
    playback_error_count: u64,
}

#[derive(Clone, Copy, Debug)]
enum DiagnosticEventBucket {
    Command,
    Playback,
}

#[derive(Clone, Copy, Debug)]
struct RealtimeSegmentPlan<'a> {
    label: &'a str,
    start_frame: u64,
    max_ticks: u64,
    source_has_audio: bool,
    frame_interval: ClockTick,
}

impl RealtimeDiagnostics {
    fn new(source: &SourceRuntime, audio_device: bool) -> Self {
        Self {
            source_id: source.source_id.clone(),
            timebase: source.timebase,
            audio_device,
            command_records: Vec::new(),
            segment_records: Vec::new(),
            command_dropped_frame_count: 0,
            playback_dropped_frame_count: 0,
            command_av_sync_warning_count: 0,
            playback_av_sync_warning_count: 0,
            playback_error_count: 0,
        }
    }

    fn record_events(
        &mut self,
        events: &[BroadcastPlayerProtocolEvent],
        bucket: DiagnosticEventBucket,
    ) {
        for event in events {
            match event {
                BroadcastPlayerProtocolEvent::DroppedFrame { .. } => match bucket {
                    DiagnosticEventBucket::Command => {
                        self.command_dropped_frame_count =
                            self.command_dropped_frame_count.saturating_add(1);
                    }
                    DiagnosticEventBucket::Playback => {
                        self.playback_dropped_frame_count =
                            self.playback_dropped_frame_count.saturating_add(1);
                    }
                },
                BroadcastPlayerProtocolEvent::AVSyncWarning { .. } => match bucket {
                    DiagnosticEventBucket::Command => {
                        self.command_av_sync_warning_count =
                            self.command_av_sync_warning_count.saturating_add(1);
                    }
                    DiagnosticEventBucket::Playback => {
                        self.playback_av_sync_warning_count =
                            self.playback_av_sync_warning_count.saturating_add(1);
                    }
                },
                BroadcastPlayerProtocolEvent::PlaybackError { .. }
                | BroadcastPlayerProtocolEvent::SourceFailed { .. } => {
                    self.playback_error_count = self.playback_error_count.saturating_add(1);
                }
                _ => {}
            }
        }
    }
}

fn run_cue_latency_diagnostics(
    runtime: &mut RunnerRuntime,
    monitor: Option<&RunnerMonitor>,
    source: &SourceRuntime,
    args: &RunnerArgs,
) -> Result<(), String> {
    let start_frame = diagnostic_cue_frame(source, args.cue_latency_frame)?;
    let frames = cue_latency_frame_sequence(
        source,
        start_frame,
        args.cue_latency_repeat,
        args.cue_latency_step,
    )?;
    let mut records = Vec::with_capacity(frames.len());
    let mut latencies_ns = Vec::with_capacity(frames.len());
    let mut totals = CueLatencyTotals {
        dropped_frame_count: 0,
        av_sync_warning_count: 0,
        playback_error_count: 0,
    };

    for (index, frame) in frames.iter().copied().enumerate() {
        let command_id = format!("cue-latency-{index:04}");
        let started_at = Instant::now();
        runtime.dispatch_at(
            PlayerRuntimeCommand::new(
                command_id.clone(),
                BroadcastPlayerProtocolCommand::CueFrame {
                    frame,
                    present_frame: true,
                },
            ),
            0,
        );
        let events = drain_runtime_events(runtime, monitor)?;
        let elapsed_ns = duration_ns(started_at.elapsed());
        let accepted = event_contains_command_accepted(&events, &command_id);
        let rejected = event_contains_command_rejected(&events, &command_id);
        let first_presented_frame = first_presented_frame(&events);
        let audio_level_event_count = event_count_audio_level(&events);
        let av_sync_warning_count = event_count_av_sync_warning(&events);
        let dropped_frame_count = event_count_dropped_frame(&events);
        let playback_error_count = event_count_playback_error(&events);
        let failed = has_failure_event(&events);

        totals.dropped_frame_count = totals
            .dropped_frame_count
            .saturating_add(dropped_frame_count as u64);
        totals.av_sync_warning_count = totals
            .av_sync_warning_count
            .saturating_add(av_sync_warning_count as u64);
        totals.playback_error_count = totals
            .playback_error_count
            .saturating_add(playback_error_count as u64);
        latencies_ns.push(elapsed_ns);

        records.push(json!({
            "index": index,
            "command_id": command_id,
            "command_name": "CueFrame",
            "frame": frame,
            "elapsed_ns": elapsed_ns,
            "accepted": accepted,
            "rejected": rejected,
            "first_presented_frame": first_presented_frame,
            "audio_level_event_count": audio_level_event_count,
            "av_sync_warning_count": av_sync_warning_count,
            "dropped_frame_count": dropped_frame_count,
            "playback_error_count": playback_error_count,
        }));

        emit_events("cue_latency", Some(index as u64), events)?;
        if failed || rejected {
            return Err(format!("cue latency command failed at frame {frame}"));
        }
        if first_presented_frame.is_none() {
            return Err(format!(
                "cue latency command did not present requested frame {frame}"
            ));
        }
    }

    emit_cue_latency_diagnostics(source, args, &frames, &latencies_ns, records, totals)
}

fn emit_cue_latency_diagnostics(
    source: &SourceRuntime,
    args: &RunnerArgs,
    frames: &[u64],
    latencies_ns: &[u64],
    records: Vec<Value>,
    totals: CueLatencyTotals,
) -> Result<(), String> {
    let min_latency_ns = latencies_ns.iter().copied().min().unwrap_or(0);
    let max_latency_ns = latencies_ns.iter().copied().max().unwrap_or(0);
    let total_latency_ns = latencies_ns.iter().fold(0_u128, |total, latency| {
        total.saturating_add(*latency as u128)
    });
    let avg_latency_ns = if latencies_ns.is_empty() {
        0
    } else {
        saturating_u128_to_u64(total_latency_ns / latencies_ns.len() as u128)
    };
    let record = json!({
        "stage": "cue_latency_diagnostics",
        "diagnostics": {
            "source_id": &source.source_id,
            "timebase": source.timebase,
            "duration_frames": source.duration_frames,
            "audio_device": args.audio_device,
            "hardware_decode": format!("{:?}", args.hardware_decode),
            "video_prefetch_frames": args.video_prefetch_frames,
            "video_cache_frames": args.video_cache_frames,
            "video_cache_bytes": args.video_cache_bytes,
            "audio_prefetch_frames": args.audio_prefetch_frames,
            "audio_cache_frames": args.audio_cache_frames,
            "audio_cache_bytes": args.audio_cache_bytes,
            "ffmpeg_read_timeout_ms": args.ffmpeg_read_timeout_ms,
            "start_frame": frames.first().copied(),
            "repeat": args.cue_latency_repeat,
            "step": args.cue_latency_step,
            "frames": frames,
            "records": records,
            "summary": {
                "min_elapsed_ns": min_latency_ns,
                "avg_elapsed_ns": avg_latency_ns,
                "max_elapsed_ns": max_latency_ns,
            },
            "totals": {
                "dropped_frame_count": totals.dropped_frame_count,
                "av_sync_warning_count": totals.av_sync_warning_count,
                "playback_error_count": totals.playback_error_count,
            }
        }
    });
    println!(
        "{}",
        serde_json::to_string(&record).map_err(|err| err.to_string())?
    );
    Ok(())
}

fn run_realtime_diagnostics(
    runtime: &mut RunnerRuntime,
    monitor: Option<&RunnerMonitor>,
    source: SourceRuntime,
    frame_interval: ClockTick,
    initial_range: FrameRange,
    args: &RunnerArgs,
) -> Result<bool, String> {
    let mut diagnostics = RealtimeDiagnostics::new(&source, args.audio_device);
    let play_events = dispatch_measured(
        runtime,
        monitor,
        "diagnostic-play",
        BroadcastPlayerProtocolCommand::Play,
        0,
        "diagnostic_play",
        &mut diagnostics,
    )?;
    if has_failure_event(&play_events) {
        return Err("diagnostic play failed".to_string());
    }
    let first_segment_boundary = run_realtime_diagnostic_segment(
        runtime,
        monitor,
        RealtimeSegmentPlan {
            label: "initial_play",
            start_frame: initial_range.start_frame,
            max_ticks: args.diagnostic_frames,
            source_has_audio: source.audio_format.is_some(),
            frame_interval,
        },
        &mut diagnostics,
    )?;

    let stop_events = dispatch_measured(
        runtime,
        monitor,
        "diagnostic-stop",
        BroadcastPlayerProtocolCommand::Stop,
        ClockTick::from(args.diagnostic_frames).saturating_mul(frame_interval),
        "diagnostic_stop",
        &mut diagnostics,
    )?;
    if has_failure_event(&stop_events) {
        return Err("diagnostic stop failed".to_string());
    }

    let seek_frame = diagnostic_seek_frame(&source, args.diagnostic_seek_frame)?;
    let seek_range = diagnostic_seek_range(&source, seek_frame, args.diagnostic_frames)?;
    let seek_request = BroadcastPlaybackRequest::new(
        format!("{}-diagnostic-seek", args.request_id),
        source.clone(),
    )?
    .with_range(seek_range)?
    .with_rate(args.rate_num, args.rate_den)?;
    let seek_events = dispatch_measured(
        runtime,
        monitor,
        "diagnostic-seek",
        BroadcastPlayerProtocolCommand::SetPlaybackRequest {
            request: Box::new(seek_request),
        },
        0,
        "diagnostic_seek",
        &mut diagnostics,
    )?;
    if has_failure_event(&seek_events) {
        return Err("diagnostic seek failed".to_string());
    }

    let replay_events = dispatch_measured(
        runtime,
        monitor,
        "diagnostic-play-after-seek",
        BroadcastPlayerProtocolCommand::Play,
        0,
        "diagnostic_play_after_seek",
        &mut diagnostics,
    )?;
    if has_failure_event(&replay_events) {
        return Err("diagnostic play after seek failed".to_string());
    }
    let seek_segment_ticks = seek_range.duration_frames().saturating_add(2);
    let seek_segment_boundary = run_realtime_diagnostic_segment(
        runtime,
        monitor,
        RealtimeSegmentPlan {
            label: "seek_play",
            start_frame: seek_range.start_frame,
            max_ticks: seek_segment_ticks,
            source_has_audio: source.audio_format.is_some(),
            frame_interval,
        },
        &mut diagnostics,
    )?;

    let reached_boundary = first_segment_boundary || seek_segment_boundary;
    emit_realtime_diagnostics(&diagnostics, frame_interval, seek_frame, reached_boundary)?;
    Ok(reached_boundary)
}

fn dispatch_measured(
    runtime: &mut RunnerRuntime,
    monitor: Option<&RunnerMonitor>,
    command_id: &str,
    command: BroadcastPlayerProtocolCommand,
    now_tick: ClockTick,
    stage: &str,
    diagnostics: &mut RealtimeDiagnostics,
) -> Result<Vec<BroadcastPlayerProtocolEvent>, String> {
    let command_name = command.command_name();
    let started_at = Instant::now();
    runtime.dispatch_at(PlayerRuntimeCommand::new(command_id, command), now_tick);
    let events = drain_runtime_events(runtime, monitor)?;
    let elapsed_ns = duration_ns(started_at.elapsed());
    diagnostics.record_events(&events, DiagnosticEventBucket::Command);
    diagnostics.command_records.push(json!({
        "command_id": command_id,
        "command_name": command_name,
        "elapsed_ns": elapsed_ns,
        "accepted": event_contains_command_accepted(&events, command_id),
        "rejected": event_contains_command_rejected(&events, command_id),
        "first_presented_frame": first_presented_frame(&events),
        "first_boundary_frame": first_boundary_frame(&events),
        "last_transport_status": last_transport_status(&events),
        "audio_level_event_count": event_count_audio_level(&events),
        "av_sync_warning_count": event_count_av_sync_warning(&events),
        "dropped_frame_count": event_count_dropped_frame(&events),
    }));
    emit_events(stage, None, events.clone())?;
    Ok(events)
}

fn run_realtime_diagnostic_segment(
    runtime: &mut RunnerRuntime,
    monitor: Option<&RunnerMonitor>,
    plan: RealtimeSegmentPlan<'_>,
    diagnostics: &mut RealtimeDiagnostics,
) -> Result<bool, String> {
    let started_at = Instant::now();
    let mut reached_boundary = false;
    let mut tick_count = 0_u64;
    let mut frame_presented_count = 0_u64;
    let mut audio_level_event_count = 0_u64;
    let mut video_ticks_without_audio_level = 0_u64;
    let mut audio_ticks_without_video_frame = 0_u64;
    let mut first_presented_frame = None;
    let mut first_presented_elapsed_ns = None;
    let mut first_audio_level_elapsed_ns = None;
    let mut max_frame_present_lateness_ns = 0_u64;
    let mut local_dropped_frame_count = 0_u64;
    let mut local_av_sync_warning_count = 0_u64;

    for tick_index in 0..plan.max_ticks {
        tick_count = tick_count.saturating_add(1);
        sleep_until_tick(
            started_at,
            plan.frame_interval
                .saturating_mul(ClockTick::from(tick_index)),
        );
        runtime.tick(started_at.elapsed().as_nanos());
        let events = drain_runtime_events(runtime, monitor)?;
        let elapsed_ns = duration_ns(started_at.elapsed());
        diagnostics.record_events(&events, DiagnosticEventBucket::Playback);
        let tick_has_video = events
            .iter()
            .any(|event| matches!(event, BroadcastPlayerProtocolEvent::FramePresented { .. }));
        let tick_has_audio = events.iter().any(|event| {
            matches!(
                event,
                BroadcastPlayerProtocolEvent::AudioLevelChanged { .. }
            )
        });

        if plan.source_has_audio && tick_has_video && !tick_has_audio {
            video_ticks_without_audio_level = video_ticks_without_audio_level.saturating_add(1);
        }
        if tick_has_audio && !tick_has_video {
            audio_ticks_without_video_frame = audio_ticks_without_video_frame.saturating_add(1);
        }

        for event in &events {
            match event {
                BroadcastPlayerProtocolEvent::FramePresented { frame } => {
                    frame_presented_count = frame_presented_count.saturating_add(1);
                    first_presented_frame.get_or_insert(*frame);
                    first_presented_elapsed_ns.get_or_insert(elapsed_ns);
                    let expected_elapsed =
                        scheduled_frame_elapsed_ns(*frame, plan.start_frame, plan.frame_interval);
                    max_frame_present_lateness_ns = max_frame_present_lateness_ns
                        .max(elapsed_ns.saturating_sub(expected_elapsed));
                }
                BroadcastPlayerProtocolEvent::AudioLevelChanged { .. } => {
                    audio_level_event_count = audio_level_event_count.saturating_add(1);
                    first_audio_level_elapsed_ns.get_or_insert(elapsed_ns);
                }
                BroadcastPlayerProtocolEvent::DroppedFrame { .. } => {
                    local_dropped_frame_count = local_dropped_frame_count.saturating_add(1);
                }
                BroadcastPlayerProtocolEvent::AVSyncWarning { .. } => {
                    local_av_sync_warning_count = local_av_sync_warning_count.saturating_add(1);
                }
                BroadcastPlayerProtocolEvent::PlaybackBoundaryReached { .. } => {
                    reached_boundary = true;
                }
                _ => {}
            }
        }

        let failed = has_failure_event(&events);
        emit_events(plan.label, Some(tick_index), events)?;
        if failed {
            return Err(format!("{} failed before completion", plan.label));
        }
        if reached_boundary {
            break;
        }
    }

    diagnostics.segment_records.push(json!({
        "label": plan.label,
        "start_frame": plan.start_frame,
        "tick_count": tick_count,
        "frame_presented_count": frame_presented_count,
        "audio_level_event_count": audio_level_event_count,
        "video_ticks_without_audio_level": video_ticks_without_audio_level,
        "audio_ticks_without_video_frame": audio_ticks_without_video_frame,
        "first_presented_frame": first_presented_frame,
        "first_presented_elapsed_ns": first_presented_elapsed_ns,
        "first_audio_level_elapsed_ns": first_audio_level_elapsed_ns,
        "max_frame_present_lateness_ns": max_frame_present_lateness_ns,
        "max_frame_present_lateness_frames": frame_lateness_count(
            max_frame_present_lateness_ns,
            plan.frame_interval,
        ),
        "dropped_frame_count": local_dropped_frame_count,
        "av_sync_warning_count": local_av_sync_warning_count,
        "reached_boundary": reached_boundary,
    }));
    Ok(reached_boundary)
}

fn emit_realtime_diagnostics(
    diagnostics: &RealtimeDiagnostics,
    frame_interval: ClockTick,
    seek_frame: u64,
    reached_boundary: bool,
) -> Result<(), String> {
    let record = json!({
        "stage": "realtime_diagnostics",
        "diagnostics": {
            "source_id": diagnostics.source_id,
            "timebase": diagnostics.timebase,
            "audio_device": diagnostics.audio_device,
            "frame_interval_ns": saturating_u128_to_u64(frame_interval),
            "seek_frame": seek_frame,
            "reached_boundary": reached_boundary,
            "command_records": diagnostics.command_records,
            "segment_records": diagnostics.segment_records,
            "totals": {
                "dropped_frame_count": diagnostics.command_dropped_frame_count
                    .saturating_add(diagnostics.playback_dropped_frame_count),
                "command_dropped_frame_count": diagnostics.command_dropped_frame_count,
                "playback_dropped_frame_count": diagnostics.playback_dropped_frame_count,
                "av_sync_warning_count": diagnostics.command_av_sync_warning_count
                    .saturating_add(diagnostics.playback_av_sync_warning_count),
                "command_av_sync_warning_count": diagnostics.command_av_sync_warning_count,
                "playback_av_sync_warning_count": diagnostics.playback_av_sync_warning_count,
                "playback_error_count": diagnostics.playback_error_count,
            }
        }
    });
    println!(
        "{}",
        serde_json::to_string(&record).map_err(|err| err.to_string())?
    );
    Ok(())
}

fn diagnostic_seek_frame(
    source: &SourceRuntime,
    requested_seek_frame: Option<u64>,
) -> Result<u64, String> {
    if source.duration_frames < 2 {
        return Err("realtime diagnostics require at least two source frames".to_string());
    }
    let max_seek_frame = source.duration_frames.saturating_sub(1);
    let seek_frame = requested_seek_frame.unwrap_or(source.duration_frames / 2);
    if seek_frame >= source.duration_frames {
        return Err(format!(
            "--diagnostic-seek-frame {seek_frame} is outside source duration {}",
            source.duration_frames
        ));
    }
    Ok(seek_frame.min(max_seek_frame))
}

fn diagnostic_cue_frame(
    source: &SourceRuntime,
    requested_cue_frame: Option<u64>,
) -> Result<u64, String> {
    if source.duration_frames == 0 {
        return Err("cue latency diagnostics require at least one source frame".to_string());
    }
    let cue_frame = requested_cue_frame.unwrap_or(source.duration_frames / 2);
    if cue_frame >= source.duration_frames {
        return Err(format!(
            "--cue-latency-frame {cue_frame} is outside source duration {}",
            source.duration_frames
        ));
    }
    Ok(cue_frame)
}

fn cue_latency_frame_sequence(
    source: &SourceRuntime,
    start_frame: u64,
    repeat: u64,
    step: u64,
) -> Result<Vec<u64>, String> {
    let mut frames = Vec::with_capacity(repeat as usize);
    for index in 0..repeat {
        let offset = index
            .checked_mul(step)
            .ok_or_else(|| "--cue-latency-repeat/step overflowed frame range".to_string())?;
        let frame = start_frame
            .checked_add(offset)
            .ok_or_else(|| "--cue-latency-repeat/step overflowed frame range".to_string())?;
        if frame >= source.duration_frames {
            return Err(format!(
                "--cue-latency-frame sequence reaches frame {frame}, outside source duration {}",
                source.duration_frames
            ));
        }
        frames.push(frame);
    }
    Ok(frames)
}

fn diagnostic_seek_range(
    source: &SourceRuntime,
    seek_frame: u64,
    diagnostic_frames: u64,
) -> Result<FrameRange, String> {
    let end_frame = seek_frame
        .saturating_add(diagnostic_frames)
        .min(source.duration_frames);
    FrameRange::new(seek_frame, end_frame)
}

fn duration_ns(duration: Duration) -> u64 {
    saturating_u128_to_u64(duration.as_nanos())
}

fn scheduled_frame_elapsed_ns(frame: u64, start_frame: u64, frame_interval: ClockTick) -> u64 {
    let frame_offset = frame.saturating_sub(start_frame);
    saturating_u128_to_u64(ClockTick::from(frame_offset).saturating_mul(frame_interval))
}

fn frame_lateness_count(lateness_ns: u64, frame_interval: ClockTick) -> u64 {
    let interval = saturating_u128_to_u64(frame_interval);
    if interval == 0 {
        return 0;
    }
    lateness_ns / interval
}

fn saturating_u128_to_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn event_contains_command_accepted(
    events: &[BroadcastPlayerProtocolEvent],
    command_id: &str,
) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            BroadcastPlayerProtocolEvent::CommandAccepted { command_id: actual, .. }
                if actual == command_id
        )
    })
}

fn event_contains_command_rejected(
    events: &[BroadcastPlayerProtocolEvent],
    command_id: &str,
) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            BroadcastPlayerProtocolEvent::CommandRejected { command_id: actual, .. }
                if actual == command_id
        )
    })
}

fn first_presented_frame(events: &[BroadcastPlayerProtocolEvent]) -> Option<u64> {
    events.iter().find_map(|event| match event {
        BroadcastPlayerProtocolEvent::FramePresented { frame } => Some(*frame),
        _ => None,
    })
}

fn first_boundary_frame(events: &[BroadcastPlayerProtocolEvent]) -> Option<u64> {
    events.iter().find_map(|event| match event {
        BroadcastPlayerProtocolEvent::PlaybackBoundaryReached { frame } => Some(*frame),
        _ => None,
    })
}

fn last_transport_status(events: &[BroadcastPlayerProtocolEvent]) -> Option<String> {
    events.iter().rev().find_map(|event| match event {
        BroadcastPlayerProtocolEvent::TransportStatusChanged { status } => {
            Some(format!("{status:?}"))
        }
        _ => None,
    })
}

fn event_count_audio_level(events: &[BroadcastPlayerProtocolEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                BroadcastPlayerProtocolEvent::AudioLevelChanged { .. }
            )
        })
        .count()
}

fn event_count_av_sync_warning(events: &[BroadcastPlayerProtocolEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, BroadcastPlayerProtocolEvent::AVSyncWarning { .. }))
        .count()
}

fn event_count_dropped_frame(events: &[BroadcastPlayerProtocolEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, BroadcastPlayerProtocolEvent::DroppedFrame { .. }))
        .count()
}

fn event_count_playback_error(events: &[BroadcastPlayerProtocolEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                BroadcastPlayerProtocolEvent::PlaybackError { .. }
                    | BroadcastPlayerProtocolEvent::SourceFailed { .. }
            )
        })
        .count()
}

#[derive(Debug)]
struct JsonlInput {
    tick: ClockTick,
    command: Option<PlayerRuntimeCommand>,
}

fn parse_jsonl_input(line: &str) -> Result<JsonlInput, String> {
    let value: Value = serde_json::from_str(line).map_err(|err| err.to_string())?;
    let tick = value.get("tick").map(parse_tick).transpose()?.unwrap_or(0);
    let Some(command_value) = value.get("command") else {
        return Ok(JsonlInput {
            tick,
            command: None,
        });
    };
    let command_id = value
        .get("command_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "stdin command line requires command_id".to_string())?;
    let command = serde_json::from_value::<BroadcastPlayerProtocolCommand>(command_value.clone())
        .map_err(|err| err.to_string())?;
    Ok(JsonlInput {
        tick,
        command: Some(PlayerRuntimeCommand::new(command_id, command)),
    })
}

fn parse_tick(value: &Value) -> Result<ClockTick, String> {
    if let Some(tick) = value.as_u64() {
        return Ok(ClockTick::from(tick));
    }
    if let Some(tick) = value.as_str() {
        return tick
            .parse::<ClockTick>()
            .map_err(|_| "tick must be an unsigned integer".to_string());
    }
    Err("tick must be an unsigned integer or string".to_string())
}

#[derive(Clone, Debug)]
struct RunnerArgs {
    path: PathBuf,
    source_id: String,
    request_id: String,
    duration_frames: Option<u64>,
    timebase: Timebase,
    timebase_explicit: bool,
    in_frame: u64,
    out_frame: Option<u64>,
    rate_num: i32,
    rate_den: u32,
    max_ticks: Option<u64>,
    output_dir: Option<PathBuf>,
    video_format: Option<VideoFormat>,
    audio_format: Option<AudioFormat>,
    registered_sources: Vec<(String, PathBuf)>,
    hardware_decode: FfmpegHardwareDecode,
    video_prefetch_frames: Option<u16>,
    video_cache_frames: Option<usize>,
    video_cache_bytes: Option<usize>,
    audio_prefetch_frames: Option<u16>,
    audio_cache_frames: Option<usize>,
    audio_cache_bytes: Option<usize>,
    ffmpeg_read_timeout_ms: Option<u64>,
    ffmpeg_bin: Option<PathBuf>,
    ffprobe_bin: Option<PathBuf>,
    audio_device: bool,
    stdin_jsonl: bool,
    probe_source_runtime: bool,
    realtime: bool,
    realtime_diagnostics: bool,
    diagnostic_frames: u64,
    diagnostic_seek_frame: Option<u64>,
    cue_latency_diagnostics: bool,
    cue_latency_frame: Option<u64>,
    cue_latency_repeat: u64,
    cue_latency_step: u64,
    require_boundary: bool,
    list_hwaccels: bool,
    emit_monitor_state: bool,
    help: bool,
}

impl RunnerArgs {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut path = None;
        let mut source_id = "runner-source".to_string();
        let mut request_id = "runner-request".to_string();
        let mut duration_frames = None;
        let mut timebase = Timebase::new(25, 1)?;
        let mut timebase_explicit = false;
        let mut in_frame = 0;
        let mut out_frame = None;
        let mut rate_num = 1;
        let mut rate_den = 1;
        let mut max_ticks = None;
        let mut output_dir = None;
        let mut video_format = None;
        let mut audio_format = None;
        let mut registered_sources = Vec::new();
        let mut hardware_decode = FfmpegHardwareDecode::Software;
        let mut video_prefetch_frames = None;
        let mut video_cache_frames = None;
        let mut video_cache_bytes = None;
        let mut audio_prefetch_frames = None;
        let mut audio_cache_frames = None;
        let mut audio_cache_bytes = None;
        let mut ffmpeg_read_timeout_ms = None;
        let mut ffmpeg_bin = None;
        let mut ffprobe_bin = None;
        let mut audio_device = false;
        let mut stdin_jsonl = false;
        let mut probe_source_runtime = false;
        let mut realtime = false;
        let mut realtime_diagnostics = false;
        let mut diagnostic_frames = 250;
        let mut diagnostic_seek_frame = None;
        let mut cue_latency_diagnostics = false;
        let mut cue_latency_frame = None;
        let mut cue_latency_repeat = 1;
        let mut cue_latency_step = 1;
        let mut require_boundary = false;
        let mut list_hwaccels = false;
        let mut emit_monitor_state = false;
        let mut help = false;

        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => help = true,
                "--path" => path = Some(PathBuf::from(next_value(&mut args, "--path")?)),
                "--source-id" => source_id = next_value(&mut args, "--source-id")?,
                "--request-id" => request_id = next_value(&mut args, "--request-id")?,
                "--duration-frames" => {
                    duration_frames = Some(parse_u64(
                        &next_value(&mut args, "--duration-frames")?,
                        "--duration-frames",
                    )?)
                }
                "--timebase" => {
                    timebase = parse_timebase(&next_value(&mut args, "--timebase")?)?;
                    timebase_explicit = true;
                }
                "--in-frame" => {
                    in_frame = parse_u64(&next_value(&mut args, "--in-frame")?, "--in-frame")?
                }
                "--out-frame" => {
                    out_frame = Some(parse_u64(
                        &next_value(&mut args, "--out-frame")?,
                        "--out-frame",
                    )?)
                }
                "--rate" => {
                    let (num, den) = parse_rate(&next_value(&mut args, "--rate")?)?;
                    rate_num = num;
                    rate_den = den;
                }
                "--max-ticks" => {
                    max_ticks = Some(parse_u64(
                        &next_value(&mut args, "--max-ticks")?,
                        "--max-ticks",
                    )?)
                }
                "--output-dir" => {
                    output_dir = Some(PathBuf::from(next_value(&mut args, "--output-dir")?))
                }
                "--audio-device" => audio_device = true,
                "--stdin-jsonl" => stdin_jsonl = true,
                "--probe-source-runtime" => probe_source_runtime = true,
                "--realtime" => realtime = true,
                "--realtime-diagnostics" => realtime_diagnostics = true,
                "--cue-latency-diagnostics" => cue_latency_diagnostics = true,
                "--require-boundary" => require_boundary = true,
                "--list-hwaccels" => list_hwaccels = true,
                "--emit-monitor-state" => emit_monitor_state = true,
                "--diagnostic-frames" => {
                    diagnostic_frames = parse_u64(
                        &next_value(&mut args, "--diagnostic-frames")?,
                        "--diagnostic-frames",
                    )?
                }
                "--diagnostic-seek-frame" => {
                    diagnostic_seek_frame = Some(parse_u64(
                        &next_value(&mut args, "--diagnostic-seek-frame")?,
                        "--diagnostic-seek-frame",
                    )?)
                }
                "--cue-latency-frame" => {
                    cue_latency_frame = Some(parse_u64(
                        &next_value(&mut args, "--cue-latency-frame")?,
                        "--cue-latency-frame",
                    )?)
                }
                "--cue-latency-repeat" => {
                    cue_latency_repeat = parse_u64(
                        &next_value(&mut args, "--cue-latency-repeat")?,
                        "--cue-latency-repeat",
                    )?
                }
                "--cue-latency-step" => {
                    cue_latency_step = parse_u64(
                        &next_value(&mut args, "--cue-latency-step")?,
                        "--cue-latency-step",
                    )?
                }
                "--register-source" => registered_sources.push(parse_source_registration(
                    &next_value(&mut args, "--register-source")?,
                )?),
                "--hwaccel" => {
                    hardware_decode = parse_hwaccel(&next_value(&mut args, "--hwaccel")?)?
                }
                "--video-prefetch-frames" => {
                    video_prefetch_frames = Some(parse_u16(
                        &next_value(&mut args, "--video-prefetch-frames")?,
                        "--video-prefetch-frames",
                    )?)
                }
                "--video-cache-frames" => {
                    video_cache_frames = Some(parse_usize(
                        &next_value(&mut args, "--video-cache-frames")?,
                        "--video-cache-frames",
                    )?)
                }
                "--video-cache-bytes" => {
                    video_cache_bytes = Some(parse_usize(
                        &next_value(&mut args, "--video-cache-bytes")?,
                        "--video-cache-bytes",
                    )?)
                }
                "--audio-prefetch-frames" => {
                    audio_prefetch_frames = Some(parse_u16(
                        &next_value(&mut args, "--audio-prefetch-frames")?,
                        "--audio-prefetch-frames",
                    )?)
                }
                "--audio-cache-frames" => {
                    audio_cache_frames = Some(parse_usize(
                        &next_value(&mut args, "--audio-cache-frames")?,
                        "--audio-cache-frames",
                    )?)
                }
                "--audio-cache-bytes" => {
                    audio_cache_bytes = Some(parse_usize(
                        &next_value(&mut args, "--audio-cache-bytes")?,
                        "--audio-cache-bytes",
                    )?)
                }
                "--ffmpeg-read-timeout-ms" => {
                    let value = parse_u64(
                        &next_value(&mut args, "--ffmpeg-read-timeout-ms")?,
                        "--ffmpeg-read-timeout-ms",
                    )?;
                    if value == 0 {
                        return Err(
                            "--ffmpeg-read-timeout-ms must be greater than zero".to_string()
                        );
                    }
                    ffmpeg_read_timeout_ms = Some(value);
                }
                "--ffmpeg-bin" => {
                    ffmpeg_bin = Some(PathBuf::from(next_value(&mut args, "--ffmpeg-bin")?))
                }
                "--ffprobe-bin" => {
                    ffprobe_bin = Some(PathBuf::from(next_value(&mut args, "--ffprobe-bin")?))
                }
                "--video-format" => {
                    video_format = Some(parse_video_format(&next_value(
                        &mut args,
                        "--video-format",
                    )?)?)
                }
                "--audio-format" => {
                    audio_format = Some(parse_audio_format(&next_value(
                        &mut args,
                        "--audio-format",
                    )?)?)
                }
                value if value.starts_with('-') => {
                    return Err(format!("unknown argument: {value}\n\n{}", usage()));
                }
                value => {
                    if path.is_some() {
                        return Err(format!("unexpected positional argument: {value}"));
                    }
                    path = Some(PathBuf::from(value));
                }
            }
        }

        if help || list_hwaccels {
            return Ok(Self {
                path: PathBuf::new(),
                source_id,
                request_id,
                duration_frames: Some(1),
                timebase,
                timebase_explicit,
                in_frame,
                out_frame,
                rate_num,
                rate_den,
                max_ticks,
                output_dir,
                video_format,
                audio_format,
                registered_sources,
                hardware_decode,
                video_prefetch_frames,
                video_cache_frames,
                video_cache_bytes,
                audio_prefetch_frames,
                audio_cache_frames,
                audio_cache_bytes,
                ffmpeg_read_timeout_ms,
                ffmpeg_bin,
                ffprobe_bin,
                audio_device,
                stdin_jsonl,
                probe_source_runtime,
                realtime,
                realtime_diagnostics,
                diagnostic_frames,
                diagnostic_seek_frame,
                cue_latency_diagnostics,
                cue_latency_frame,
                cue_latency_repeat,
                cue_latency_step,
                require_boundary,
                list_hwaccels,
                emit_monitor_state,
                help,
            });
        }

        let path = path.ok_or_else(|| format!("missing --path\n\n{}", usage()))?;
        if !path.exists() {
            return Err(format!("media path does not exist: {}", path.display()));
        }
        for (source_id, source_path) in &registered_sources {
            if !source_path.exists() {
                return Err(format!(
                    "registered source path does not exist for {source_id}: {}",
                    source_path.display()
                ));
            }
        }
        if duration_frames == Some(0) {
            return Err("--duration-frames must be greater than zero".to_string());
        }
        if !probe_source_runtime {
            if duration_frames.is_none() {
                return Err(
                    "--duration-frames is required unless --probe-source-runtime is used"
                        .to_string(),
                );
            }
            if !timebase_explicit {
                return Err(
                    "--timebase is required unless --probe-source-runtime is used".to_string(),
                );
            }
            if video_format.is_none() && audio_format.is_none() {
                return Err(
                    "at least one of --video-format or --audio-format is required unless --probe-source-runtime is used"
                        .to_string(),
                );
            }
        }
        if rate_den == 0 {
            return Err("--rate denominator must be greater than zero".to_string());
        }
        if diagnostic_frames == 0 {
            return Err("--diagnostic-frames must be greater than zero".to_string());
        }
        if cue_latency_repeat == 0 {
            return Err("--cue-latency-repeat must be greater than zero".to_string());
        }
        if stdin_jsonl && realtime_diagnostics {
            return Err(
                "--stdin-jsonl and --realtime-diagnostics cannot be used together".to_string(),
            );
        }
        if stdin_jsonl && cue_latency_diagnostics {
            return Err(
                "--stdin-jsonl and --cue-latency-diagnostics cannot be used together".to_string(),
            );
        }
        if realtime_diagnostics && cue_latency_diagnostics {
            return Err(
                "--realtime-diagnostics and --cue-latency-diagnostics cannot be used together"
                    .to_string(),
            );
        }
        Ok(Self {
            path,
            source_id,
            request_id,
            duration_frames,
            timebase,
            timebase_explicit,
            in_frame,
            out_frame,
            rate_num,
            rate_den,
            max_ticks,
            output_dir,
            video_format,
            audio_format,
            registered_sources,
            hardware_decode,
            video_prefetch_frames,
            video_cache_frames,
            video_cache_bytes,
            audio_prefetch_frames,
            audio_cache_frames,
            audio_cache_bytes,
            ffmpeg_read_timeout_ms,
            ffmpeg_bin,
            ffprobe_bin,
            audio_device,
            stdin_jsonl,
            probe_source_runtime,
            realtime,
            realtime_diagnostics,
            diagnostic_frames,
            diagnostic_seek_frame,
            cue_latency_diagnostics,
            cue_latency_frame,
            cue_latency_repeat,
            cue_latency_step,
            require_boundary,
            list_hwaccels,
            emit_monitor_state,
            help,
        })
    }
}

fn source_runtime_from_args(
    args: &RunnerArgs,
    toolchain: &FfmpegToolchain,
) -> Result<SourceRuntime, String> {
    let mut source = if args.probe_source_runtime {
        probe_source_runtime_with_toolchain(
            &args.path,
            args.source_id.clone(),
            args.timebase_explicit.then_some(args.timebase),
            toolchain,
        )
        .map_err(|err| err.to_string())?
        .source
    } else {
        SourceRuntime::new(
            args.source_id.clone(),
            args.duration_frames
                .ok_or_else(|| "--duration-frames is required".to_string())?,
            args.timebase,
        )?
    };
    if args.timebase_explicit {
        source.timebase = args.timebase;
    }
    if let Some(duration_frames) = args.duration_frames {
        source.duration_frames = duration_frames;
    }
    if let Some(video_format) = args.video_format.clone() {
        source.video_format = Some(video_format);
    }
    if let Some(audio_format) = args.audio_format.clone() {
        source.audio_format = Some(audio_format);
    }
    if source.video_format.is_none() && source.audio_format.is_none() {
        return Err("source has no declared video/audio format".to_string());
    }
    Ok(source)
}

fn source_registry(args: &RunnerArgs) -> FfmpegSourceRegistry {
    let mut source_paths = BTreeMap::from([(args.source_id.clone(), args.path.clone())]);
    for (source_id, path) in &args.registered_sources {
        source_paths.insert(source_id.clone(), path.clone());
    }
    FfmpegSourceRegistry::new(source_paths)
}

fn emit_hardware_decode_backends(toolchain: &FfmpegToolchain) -> Result<(), String> {
    let backends = available_hardware_decode_backends_with_toolchain(toolchain)
        .map_err(|err| err.to_string())?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "hardware_decode_backends": backends,
        }))
        .map_err(|err| err.to_string())?
    );
    Ok(())
}

fn emit_events(
    stage: &str,
    tick_index: Option<u64>,
    events: Vec<BroadcastPlayerProtocolEvent>,
) -> Result<(), String> {
    for event in events {
        let record = json!({
            "stage": stage,
            "tick": tick_index,
            "event": event,
        });
        println!(
            "{}",
            serde_json::to_string(&record).map_err(|err| err.to_string())?
        );
    }
    Ok(())
}

fn drain_runtime_events(
    runtime: &mut RunnerRuntime,
    monitor: Option<&RunnerMonitor>,
) -> Result<Vec<BroadcastPlayerProtocolEvent>, String> {
    let events = runtime.drain_events();
    if let Some(monitor) = monitor {
        monitor.apply_events(&events)?;
    }
    Ok(events)
}

fn emit_final_state(runtime: &RunnerRuntime, reached_boundary: bool) -> Result<(), String> {
    let record = json!({
        "stage": "final_state",
        "reached_boundary": reached_boundary,
        "state": runtime.transport().state(),
    });
    println!(
        "{}",
        serde_json::to_string(&record).map_err(|err| err.to_string())?
    );
    Ok(())
}

fn emit_monitor_state(monitor: &RunnerMonitor) -> Result<(), String> {
    let state = monitor.snapshot()?;
    let last_frame_buffer = state.last_frame_buffer.as_ref().map(|frame_buffer| {
        json!({
            "source_id": frame_buffer.source_id,
            "frame": frame_buffer.frame,
            "byte_len": frame_buffer.bytes.len(),
            "pixel_layout": format!("{:?}", frame_buffer.pixel_layout),
            "video_format": frame_buffer.video_format,
        })
    });
    let record = json!({
        "stage": "monitor_state",
        "state": {
            "ready_source_id": state.ready_source_id,
            "preloaded_source_id": state.preloaded_source_id,
            "active_source_id": state.active_source_id,
            "failed_source_id": state.failed_source_id,
            "revised_source_id": state.revised_source_id,
            "source_revision": state.source_revision,
            "carrier_frame": state.carrier_frame,
            "presented_frame": state.presented_frame,
            "boundary_frame": state.boundary_frame,
            "transport_status": state.transport_status,
            "timebase": state.timebase,
            "video_format": state.video_format,
            "drop_frame_mode": state.drop_frame_mode,
            "buffered_frames": state.buffered_frames,
            "last_frame_buffer": last_frame_buffer,
            "expected_dropped_frame": state.expected_dropped_frame,
            "audio_levels": state.audio_levels,
            "audio_runtime": state.audio_runtime,
            "av_sync_offset_frames": state.av_sync_offset_frames,
            "last_warning": state.last_warning,
            "last_error": state.last_error,
            "event_revision": state.event_revision,
            "frame_revision": state.frame_revision,
        }
    });
    println!(
        "{}",
        serde_json::to_string(&record).map_err(|err| err.to_string())?
    );
    Ok(())
}

fn has_boundary_event(events: &[BroadcastPlayerProtocolEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            BroadcastPlayerProtocolEvent::PlaybackBoundaryReached { .. }
        )
    })
}

fn has_failure_event(events: &[BroadcastPlayerProtocolEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            BroadcastPlayerProtocolEvent::SourceFailed { .. }
                | BroadcastPlayerProtocolEvent::PlaybackError { .. }
        )
    })
}

fn build_audio_sink(
    output_dir: Option<&PathBuf>,
    audio_device: bool,
) -> Result<FfmpegAudioSink, String> {
    if audio_device {
        return open_audio_device_sink();
    }
    Ok(output_dir
        .map(|dir| FfmpegAudioSink::raw_file(dir.join("audio")))
        .unwrap_or_default())
}

#[cfg(feature = "audio-device")]
fn open_audio_device_sink() -> Result<FfmpegAudioSink, String> {
    FfmpegAudioSink::audio_device().map_err(|err| err.to_string())
}

#[cfg(not(feature = "audio-device"))]
fn open_audio_device_sink() -> Result<FfmpegAudioSink, String> {
    Err("--audio-device requires qnc-player-runner audio-device feature".to_string())
}

fn frame_interval_ticks(
    timebase: Timebase,
    rate_num: i32,
    rate_den: u32,
) -> Result<ClockTick, String> {
    let config = FrameClockConfig::new(timebase, FrameClockRate::new(rate_num, rate_den)?);
    config
        .frame_interval_ticks()
        .ok_or_else(|| "rate 0 has no frame interval".to_string())
}

fn parse_timebase(value: &str) -> Result<Timebase, String> {
    let (num, den) = parse_unsigned_ratio(value, "--timebase")?;
    Timebase::new(num, den)
}

fn parse_rate(value: &str) -> Result<(i32, u32), String> {
    let Some((num, den)) = value.split_once('/') else {
        return Ok((parse_i32(value, "--rate")?, 1));
    };
    Ok((parse_i32(num, "--rate")?, parse_u32(den, "--rate")?))
}

fn parse_video_format(value: &str) -> Result<VideoFormat, String> {
    let Some((width, height)) = value.split_once('x') else {
        return Err("--video-format must use <width>x<height>".to_string());
    };
    VideoFormat::new(
        parse_u32(width, "--video-format")?,
        parse_u32(height, "--video-format")?,
        FieldMode::Progressive,
        ColorSpace::Rec709,
    )
}

fn parse_audio_format(value: &str) -> Result<AudioFormat, String> {
    let Some((sample_rate_hz, channel_count)) = value.split_once('x') else {
        return Err("--audio-format must use <sample-rate>x<channels>".to_string());
    };
    AudioFormat::new(
        parse_u32(sample_rate_hz, "--audio-format")?,
        parse_u16(channel_count, "--audio-format")?,
    )
}

fn parse_hwaccel(value: &str) -> Result<FfmpegHardwareDecode, String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "none" | "software" => Ok(FfmpegHardwareDecode::Software),
        "auto" => Ok(FfmpegHardwareDecode::Auto),
        "cuda" | "cuvid" | "qsv" | "vaapi" | "dxva2" | "d3d11va" | "videotoolbox"
        | "vdpau" => FfmpegHardwareDecode::backend(normalized),
        _ => Err(
            "--hwaccel must be one of: none, auto, cuda, cuvid, qsv, vaapi, dxva2, d3d11va, videotoolbox, vdpau"
                .to_string(),
        ),
    }
}

fn parse_source_registration(value: &str) -> Result<(String, PathBuf), String> {
    let Some((source_id, path)) = value.split_once('=') else {
        return Err("--register-source must use <source-id>=<path>".to_string());
    };
    if source_id.trim().is_empty() {
        return Err("--register-source source id must not be blank".to_string());
    }
    if path.trim().is_empty() {
        return Err("--register-source path must not be blank".to_string());
    }
    Ok((source_id.to_string(), PathBuf::from(path)))
}

fn parse_unsigned_ratio(value: &str, name: &str) -> Result<(u32, u32), String> {
    let Some((num, den)) = value.split_once('/') else {
        return Ok((parse_u32(value, name)?, 1));
    };
    Ok((parse_u32(num, name)?, parse_u32(den, name)?))
}

fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

fn parse_u32(value: &str, name: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

fn parse_u16(value: &str, name: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

fn parse_usize(value: &str, name: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

fn parse_i32(value: &str, name: &str) -> Result<i32, String> {
    value
        .parse::<i32>()
        .map_err(|_| format!("{name} must be an integer"))
}

fn next_value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn runner_toolchain(args: &RunnerArgs) -> Result<FfmpegToolchain, String> {
    let default_toolchain = FfmpegToolchain::default();
    let ffmpeg = args
        .ffmpeg_bin
        .clone()
        .unwrap_or_else(|| default_toolchain.ffmpeg().to_path_buf());
    let ffprobe = args
        .ffprobe_bin
        .clone()
        .unwrap_or_else(|| default_toolchain.ffprobe().to_path_buf());
    FfmpegToolchain::new(ffmpeg, ffprobe)
}

fn require_tool(tool: &std::path::Path, label: &str) -> Result<(), String> {
    let mut child = ProcessCommand::new(tool)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("{label} is required at {}: {err}", tool.display()))?;
    match wait_for_runner_child_exit(&mut child, RUNNER_TOOL_CHECK_TIMEOUT) {
        Ok(Some(status)) if status.success() => Ok(()),
        Ok(Some(status)) => Err(format!(
            "{label} is required at {}; version check exited with {status}",
            tool.display()
        )),
        Ok(None) => {
            let cleanup = terminate_runner_child_bounded(child, label);
            Err(format!(
                "{label} version check timed out after {} ms at {}; {cleanup}",
                RUNNER_TOOL_CHECK_TIMEOUT.as_millis(),
                tool.display()
            ))
        }
        Err(err) => {
            let cleanup = terminate_runner_child_bounded(child, label);
            Err(format!(
                "{label} version check failed at {}: {err}; {cleanup}",
                tool.display()
            ))
        }
    }
}

fn wait_for_runner_child_exit(
    child: &mut Child,
    timeout: Duration,
) -> Result<Option<std::process::ExitStatus>, std::io::Error> {
    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) if started_at.elapsed() >= timeout => return Ok(None),
            Ok(None) => thread::sleep(RUNNER_TOOL_CHECK_POLL),
            Err(err) => return Err(err),
        }
    }
}

fn terminate_runner_child_bounded(mut child: Child, label: &str) -> String {
    let kill_message = match child.kill() {
        Ok(()) => format!("{label} kill requested"),
        Err(err) => format!("{label} kill failed: {err}"),
    };
    match wait_for_runner_child_exit(&mut child, RUNNER_TOOL_TERMINATE_TIMEOUT) {
        Ok(Some(status)) => format!("{kill_message}; exited with {status}"),
        Ok(None) => {
            match thread::Builder::new()
                .name("qnc-runner-tool-reaper".to_string())
                .spawn(move || {
                    let _ = child.wait();
                }) {
                Ok(_handle) => format!("{kill_message}; wait deferred to reaper"),
                Err(spawn_err) => format!("{kill_message}; reaper spawn failed: {spawn_err}"),
            }
        }
        Err(wait_err) => format!("{kill_message}; wait failed: {wait_err}"),
    }
}

fn usage() -> &'static str {
    "Usage:
  qnc-player-runner --path <media-file> [options]

Options:
  --source-id <id>           Runtime source id. Default: runner-source
  --request-id <id>          Runtime request id. Default: runner-request
  --duration-frames <frames> Required runtime duration unless --probe-source-runtime is used.
  --timebase <num[/den]>     Required frame timebase unless --probe-source-runtime is used.
  --in-frame <frame>         Execution range start frame. Default: 0
  --out-frame <frame>        Execution range end frame. Default: duration-frames
  --rate <num[/den]>         Playback rate. Default: 1/1
  --max-ticks <count>        Max scheduler ticks before exit.
  --output-dir <dir>         Optional raw output root for decoded RGB/audio packets.
  --audio-device             Send decoded audio packets to the default output device.
  --list-hwaccels            Print available FFmpeg hardware decode backends as JSON and exit.
  --register-source <id=path> Add an extra source path to the runner adapter registry.
  --hwaccel <mode>           Optional FFmpeg hardware decode: none, auto, cuda, cuvid, qsv, vaapi, dxva2, d3d11va, videotoolbox, vdpau.
  --video-prefetch-frames <n> Video pipe read-ahead window on cache miss. Adapter default when omitted.
  --video-cache-frames <n>    Max decoded video frames kept in adapter cache. Adapter default when omitted.
  --video-cache-bytes <n>     Max decoded video bytes kept in adapter cache. Adapter default when omitted.
  --audio-prefetch-frames <n> Audio pipe read-ahead window on cache miss. Adapter default when omitted.
  --audio-cache-frames <n>    Max decoded audio frame packets kept in adapter cache. Adapter default when omitted.
  --audio-cache-bytes <n>     Max decoded audio bytes kept in adapter cache. Adapter default when omitted.
  --ffmpeg-read-timeout-ms <n> Max wait for one FFmpeg pipe read before killing the adapter process. Adapter default when omitted.
  --ffmpeg-bin <path>         FFmpeg executable path. Defaults to QNC_FFMPEG or ffmpeg from PATH.
  --ffprobe-bin <path>        FFprobe executable path. Defaults to QNC_FFPROBE or ffprobe from PATH.
  --stdin-jsonl              Read neutral protocol command/tick JSON lines from stdin after initial request setup.
  --probe-source-runtime     Explicit FFprobe helper for standalone diagnostics; not default playback path.
  --realtime                 Use wall-clock scheduling between frame ticks.
  --realtime-diagnostics     Run measured Play/Stop/seek/Play realtime diagnostics.
  --diagnostic-frames <n>    Realtime diagnostic tick count per play segment. Default: 250.
  --diagnostic-seek-frame <n> Frame used by diagnostic seek SetPlaybackRequest. Default: source midpoint.
  --cue-latency-diagnostics  Run measured CueFrame diagnostics without UI.
  --cue-latency-frame <n>    First frame used by CueFrame diagnostics. Default: source midpoint.
  --cue-latency-repeat <n>   Number of CueFrame commands to measure. Default: 1.
  --cue-latency-step <n>     Frame step between measured CueFrame commands. Default: 1; use 0 for same-frame cache hit.
  --require-boundary         Exit with failure if playback does not reach the execution boundary.
  --emit-monitor-state       Emit final monitor projection state after playback.
  --video-format <w>x<h>     Declares a video track, Rec709 progressive.
  --audio-format <rate>x<c>  Declares an audio track.
  --help                    Show this help.

Example:
  cargo run -p qnc-player-runner -- --path C:\\media\\source-file --duration-frames 250 --timebase 25/1 --video-format 1920x1080 --audio-format 48000x2 --max-ticks 20"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hwaccel_defaults_to_software_modes() {
        assert_eq!(
            parse_hwaccel("none").unwrap(),
            FfmpegHardwareDecode::Software
        );
        assert_eq!(
            parse_hwaccel("software").unwrap(),
            FfmpegHardwareDecode::Software
        );
    }

    #[test]
    fn parse_hwaccel_accepts_auto_and_named_backends() {
        assert_eq!(parse_hwaccel("auto").unwrap(), FfmpegHardwareDecode::Auto);
        assert_eq!(
            parse_hwaccel("CUDA").unwrap(),
            FfmpegHardwareDecode::Backend("cuda".to_string())
        );
    }

    #[test]
    fn parse_hwaccel_rejects_unknown_backend() {
        let err = parse_hwaccel("unknown").unwrap_err();

        assert!(err.contains("--hwaccel must be one of"));
    }

    #[test]
    fn runner_args_accept_decode_cache_options() {
        let temp_path = std::env::temp_dir().join("qnc-runner-arg-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let args = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "10".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--video-format".to_string(),
            "160x90".to_string(),
            "--hwaccel".to_string(),
            "auto".to_string(),
            "--video-prefetch-frames".to_string(),
            "12".to_string(),
            "--video-cache-frames".to_string(),
            "48".to_string(),
            "--video-cache-bytes".to_string(),
            "1024".to_string(),
            "--audio-prefetch-frames".to_string(),
            "16".to_string(),
            "--audio-cache-frames".to_string(),
            "64".to_string(),
            "--audio-cache-bytes".to_string(),
            "2048".to_string(),
            "--ffmpeg-read-timeout-ms".to_string(),
            "2500".to_string(),
            "--ffmpeg-bin".to_string(),
            "tools/ffmpeg-custom".to_string(),
            "--ffprobe-bin".to_string(),
            "tools/ffprobe-custom".to_string(),
            "--stdin-jsonl".to_string(),
        ])
        .unwrap();

        assert_eq!(args.hardware_decode, FfmpegHardwareDecode::Auto);
        assert_eq!(args.video_prefetch_frames, Some(12));
        assert_eq!(args.video_cache_frames, Some(48));
        assert_eq!(args.video_cache_bytes, Some(1024));
        assert_eq!(args.audio_prefetch_frames, Some(16));
        assert_eq!(args.audio_cache_frames, Some(64));
        assert_eq!(args.audio_cache_bytes, Some(2048));
        assert_eq!(args.ffmpeg_read_timeout_ms, Some(2500));
        assert_eq!(args.ffmpeg_bin, Some(PathBuf::from("tools/ffmpeg-custom")));
        assert_eq!(
            args.ffprobe_bin,
            Some(PathBuf::from("tools/ffprobe-custom"))
        );
        assert!(args.stdin_jsonl);

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn runner_toolchain_rejects_blank_explicit_tool_path() {
        let temp_path = std::env::temp_dir().join("qnc-runner-toolchain-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let args = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "10".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--video-format".to_string(),
            "160x90".to_string(),
            "--ffmpeg-bin".to_string(),
            " ".to_string(),
        ])
        .unwrap();

        let err = runner_toolchain(&args).unwrap_err();

        assert!(err.contains("ffmpeg path must not be blank"));

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn runner_args_accept_extra_source_registration() {
        let temp_path = std::env::temp_dir().join("qnc-runner-primary-source.tmp");
        let next_path = std::env::temp_dir().join("qnc-runner-next-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();
        std::fs::write(&next_path, b"fixture").unwrap();

        let args = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "10".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--video-format".to_string(),
            "160x90".to_string(),
            "--register-source".to_string(),
            format!("src-b={}", next_path.display()),
        ])
        .unwrap();

        assert_eq!(args.registered_sources.len(), 1);
        assert_eq!(args.registered_sources[0].0, "src-b");
        assert_eq!(args.registered_sources[0].1, next_path);

        let _ = std::fs::remove_file(temp_path);
        let _ = std::fs::remove_file(next_path);
    }

    #[test]
    fn runner_args_reject_zero_ffmpeg_read_timeout() {
        let temp_path = std::env::temp_dir().join("qnc-runner-zero-timeout-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let err = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "10".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--video-format".to_string(),
            "160x90".to_string(),
            "--ffmpeg-read-timeout-ms".to_string(),
            "0".to_string(),
        ])
        .unwrap_err();

        assert!(err.contains("--ffmpeg-read-timeout-ms must be greater than zero"));

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn runner_args_accept_realtime_boundary_test_flags() {
        let temp_path = std::env::temp_dir().join("qnc-runner-realtime-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let args = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "10".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--audio-format".to_string(),
            "48000x1".to_string(),
            "--realtime".to_string(),
            "--require-boundary".to_string(),
        ])
        .unwrap();

        assert!(args.realtime);
        assert!(args.require_boundary);

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn runner_args_accept_realtime_diagnostics_flags() {
        let temp_path = std::env::temp_dir().join("qnc-runner-realtime-diagnostics-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let args = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "100".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--video-format".to_string(),
            "160x90".to_string(),
            "--audio-format".to_string(),
            "48000x1".to_string(),
            "--realtime-diagnostics".to_string(),
            "--diagnostic-frames".to_string(),
            "12".to_string(),
            "--diagnostic-seek-frame".to_string(),
            "40".to_string(),
        ])
        .unwrap();

        assert!(args.realtime_diagnostics);
        assert_eq!(args.diagnostic_frames, 12);
        assert_eq!(args.diagnostic_seek_frame, Some(40));

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn runner_args_accept_cue_latency_diagnostics_flags() {
        let temp_path = std::env::temp_dir().join("qnc-runner-cue-latency-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let args = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "100".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--video-format".to_string(),
            "160x90".to_string(),
            "--cue-latency-diagnostics".to_string(),
            "--cue-latency-frame".to_string(),
            "40".to_string(),
            "--cue-latency-repeat".to_string(),
            "3".to_string(),
            "--cue-latency-step".to_string(),
            "2".to_string(),
        ])
        .unwrap();

        assert!(args.cue_latency_diagnostics);
        assert_eq!(args.cue_latency_frame, Some(40));
        assert_eq!(args.cue_latency_repeat, 3);
        assert_eq!(args.cue_latency_step, 2);

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn runner_args_reject_jsonl_with_cue_latency_diagnostics() {
        let temp_path = std::env::temp_dir().join("qnc-runner-jsonl-cue-latency-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let err = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "100".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--video-format".to_string(),
            "160x90".to_string(),
            "--stdin-jsonl".to_string(),
            "--cue-latency-diagnostics".to_string(),
        ])
        .unwrap_err();

        assert!(err.contains("--stdin-jsonl and --cue-latency-diagnostics"));

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn runner_args_reject_realtime_with_cue_latency_diagnostics() {
        let temp_path = std::env::temp_dir().join("qnc-runner-realtime-cue-latency-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let err = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "100".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--video-format".to_string(),
            "160x90".to_string(),
            "--realtime-diagnostics".to_string(),
            "--cue-latency-diagnostics".to_string(),
        ])
        .unwrap_err();

        assert!(err.contains("--realtime-diagnostics and --cue-latency-diagnostics"));

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn cue_latency_frame_sequence_is_frame_based() {
        let source = SourceRuntime::new("src", 10, Timebase::new(25, 1).unwrap()).unwrap();

        assert_eq!(
            cue_latency_frame_sequence(&source, 3, 3, 2).unwrap(),
            vec![3, 5, 7]
        );
        assert_eq!(
            cue_latency_frame_sequence(&source, 3, 3, 0).unwrap(),
            vec![3, 3, 3]
        );
        assert!(
            cue_latency_frame_sequence(&source, 8, 3, 1)
                .unwrap_err()
                .contains("outside source duration")
        );
    }

    #[test]
    fn runner_args_reject_jsonl_with_realtime_diagnostics() {
        let temp_path =
            std::env::temp_dir().join("qnc-runner-jsonl-realtime-diagnostics-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let err = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "100".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--video-format".to_string(),
            "160x90".to_string(),
            "--stdin-jsonl".to_string(),
            "--realtime-diagnostics".to_string(),
        ])
        .unwrap_err();

        assert!(err.contains("--stdin-jsonl and --realtime-diagnostics"));

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn runner_args_accept_monitor_state_flag() {
        let temp_path = std::env::temp_dir().join("qnc-runner-monitor-source.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let args = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--duration-frames".to_string(),
            "10".to_string(),
            "--timebase".to_string(),
            "25/1".to_string(),
            "--video-format".to_string(),
            "160x90".to_string(),
            "--emit-monitor-state".to_string(),
        ])
        .unwrap();

        assert!(args.emit_monitor_state);

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn runner_args_accept_hwaccel_listing_without_media_path() {
        let args = RunnerArgs::parse(["--list-hwaccels".to_string()]).unwrap();

        assert!(args.list_hwaccels);
        assert!(args.path.as_os_str().is_empty());
    }

    #[test]
    fn runner_args_require_prepared_runtime_metadata_without_probe() {
        let temp_path = std::env::temp_dir().join("qnc-runner-missing-source-runtime.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let err = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
        ])
        .unwrap_err();

        assert!(err.contains("--duration-frames"));

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn runner_args_allow_explicit_probe_source_runtime() {
        let temp_path = std::env::temp_dir().join("qnc-runner-probe-source-runtime.tmp");
        std::fs::write(&temp_path, b"fixture").unwrap();

        let args = RunnerArgs::parse([
            "--path".to_string(),
            temp_path.to_string_lossy().into_owned(),
            "--probe-source-runtime".to_string(),
        ])
        .unwrap();

        assert!(args.probe_source_runtime);

        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn jsonl_input_accepts_protocol_command_with_integer_tick() {
        let input =
            parse_jsonl_input(r#"{"command_id":"cmd-play","command":"Play","tick":40}"#).unwrap();

        assert_eq!(input.tick, 40);
        assert!(matches!(
            input.command.unwrap().command,
            BroadcastPlayerProtocolCommand::Play
        ));
    }

    #[test]
    fn jsonl_input_accepts_tick_only_line() {
        let input = parse_jsonl_input(r#"{"tick":"80"}"#).unwrap();

        assert_eq!(input.tick, 80);
        assert!(input.command.is_none());
    }

    #[test]
    fn jsonl_input_rejects_command_without_command_id() {
        let err = parse_jsonl_input(r#"{"command":"Stop","tick":0}"#).unwrap_err();

        assert!(err.contains("command_id"));
    }
}
