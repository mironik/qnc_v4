use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{io::Write, process::Child};

use serde_json::{Value, json};

#[test]
fn runner_decodes_real_file_and_writes_external_output() {
    assert_runner_decodes_av_fixture(RealRunnerFixture::create_av);
}

#[test]
fn runner_decodes_real_mov_file_and_writes_external_output() {
    assert_runner_decodes_av_fixture(RealRunnerFixture::create_mov_av);
}

#[test]
fn runner_decodes_real_mxf_file_and_writes_external_output() {
    assert_runner_decodes_av_fixture(RealRunnerFixture::create_mxf_av);
}

#[test]
fn runner_decodes_real_mxf_corpus_when_configured() {
    let Some(root) = std::env::var_os("QNC_REAL_MXF_CORPUS_DIR") else {
        eprintln!("set QNC_REAL_MXF_CORPUS_DIR to run the real MXF corpus smoke");
        return;
    };
    require_runner_tools();
    let root = PathBuf::from(root);
    let paths = collect_media_files(&root, &["mxf"]);
    assert!(
        !paths.is_empty(),
        "QNC_REAL_MXF_CORPUS_DIR contains no MXF files: {}",
        root.display()
    );

    for path in paths {
        let stdout = run_existing_source_with_probe(&path);
        assert!(
            stdout_contains_event(&stdout, "FramePresented", Some(1)),
            "runner did not present frame 1 for {}",
            path.display()
        );
        assert!(
            stdout_contains_event(&stdout, "PlaybackBoundaryReached", Some(1)),
            "runner did not reach probe boundary for {}",
            path.display()
        );
    }
}

#[test]
fn runner_decodes_real_mpeg_ts_file_and_writes_external_output() {
    assert_runner_decodes_av_fixture(RealRunnerFixture::create_mpeg_ts_av);
}

#[test]
fn runner_decodes_real_file_to_default_source_boundary_out() {
    require_runner_tools();
    let fixture = RealRunnerFixture::create_av();
    let output_dir = fixture.dir.join("out");
    let stdout = run_runner(
        &fixture,
        &output_dir,
        RunnerSmokeArgs {
            duration_frames: "50",
            timebase: "25/1",
            video_format: Some("160x90"),
            audio_format: Some("48000x1"),
            out_frame: None,
            max_ticks: "54",
        },
    );

    assert!(stdout_contains_event(&stdout, "FramePresented", Some(50)));
    assert!(stdout_contains_event(
        &stdout,
        "PlaybackBoundaryReached",
        Some(50)
    ));
    assert_eq!(file_count(&output_dir.join("video"), "rgb"), 51);
    assert_eq!(file_count(&output_dir.join("audio"), "s16le"), 51);
}

#[test]
fn runner_decodes_longer_real_file_to_default_source_boundary_out() {
    require_runner_tools();
    let fixture = RealRunnerFixture::create_long_av();
    let output_dir = fixture.dir.join("long-out");
    let stdout = run_runner(
        &fixture,
        &output_dir,
        RunnerSmokeArgs {
            duration_frames: "250",
            timebase: "25/1",
            video_format: Some("160x90"),
            audio_format: Some("48000x1"),
            out_frame: None,
            max_ticks: "254",
        },
    );

    assert!(stdout_contains_event(&stdout, "FramePresented", Some(250)));
    assert!(stdout_contains_event(
        &stdout,
        "PlaybackBoundaryReached",
        Some(250)
    ));
    assert_eq!(file_count(&output_dir.join("video"), "rgb"), 251);
    assert_eq!(file_count(&output_dir.join("audio"), "s16le"), 251);
}

#[test]
fn runner_realtime_mode_reaches_boundary_with_real_file() {
    require_runner_tools();
    let fixture = RealRunnerFixture::create_av();
    let output_dir = fixture.dir.join("realtime-out");
    let stdout = run_runner_realtime_boundary(&fixture, &output_dir);

    assert!(stdout_contains_event(&stdout, "FramePresented", Some(1)));
    assert!(stdout_contains_event(
        &stdout,
        "PlaybackBoundaryReached",
        Some(1)
    ));
    assert_eq!(file_count(&output_dir.join("video"), "rgb"), 2);
    assert_eq!(file_count(&output_dir.join("audio"), "s16le"), 2);
}

#[test]
fn runner_emits_realtime_diagnostics_with_real_file() {
    require_runner_tools();
    let fixture = RealRunnerFixture::create_av();
    let output_dir = fixture.dir.join("realtime-diagnostics-out");
    let stdout = run_runner_realtime_diagnostics(&fixture, &output_dir);

    assert!(stdout_contains_event(&stdout, "FramePresented", Some(10)));
    assert!(stdout_contains_event(
        &stdout,
        "PlaybackBoundaryReached",
        Some(12)
    ));
    assert!(stdout_contains_realtime_diagnostics(&stdout, 10));
}

#[test]
fn runner_uses_explicit_probe_source_runtime_when_requested() {
    require_runner_tools();
    let fixture = RealRunnerFixture::create_av();
    let output_dir = fixture.dir.join("probe-out");
    let stdout = run_runner_with_probe(&fixture, &output_dir);

    assert!(stdout_contains_event(&stdout, "FramePresented", Some(4)));
    assert!(stdout_contains_event(
        &stdout,
        "PlaybackBoundaryReached",
        Some(4)
    ));
    assert_eq!(file_count(&output_dir.join("video"), "rgb"), 5);
    assert_eq!(file_count(&output_dir.join("audio"), "s16le"), 5);
}

#[test]
fn runner_emits_monitor_state_from_event_and_frame_bridge() {
    require_runner_tools();
    let fixture = RealRunnerFixture::create_av();
    let output_dir = fixture.dir.join("monitor-out");
    let stdout = run_runner_with_monitor(&fixture, &output_dir);

    assert!(stdout_contains_event(&stdout, "FramePresented", Some(4)));
    assert!(stdout_contains_event(
        &stdout,
        "PlaybackBoundaryReached",
        Some(4)
    ));
    assert!(stdout_contains_monitor_state(&stdout, 4, 160 * 90 * 3));
    assert_eq!(file_count(&output_dir.join("video"), "rgb"), 5);
    assert_eq!(file_count(&output_dir.join("audio"), "s16le"), 5);
}

#[test]
fn runner_rejects_missing_source_runtime_without_explicit_probe() {
    require_runner_tools();
    let fixture = RealRunnerFixture::create_av();
    let output = Command::new(env!("CARGO_BIN_EXE_qnc-player-runner"))
        .args(["--path", fixture.path.to_str().unwrap()])
        .output()
        .expect("runner should execute");

    assert!(
        !output.status.success(),
        "runner should reject missing source runtime metadata"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--duration-frames"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn runner_accepts_stdin_jsonl_protocol_commands() {
    require_runner_tools();
    let fixture = RealRunnerFixture::create_av();
    let output_dir = fixture.dir.join("jsonl-out");
    let mut child = spawn_runner_jsonl(&fixture, &output_dir);

    {
        let stdin = child.stdin.as_mut().expect("runner stdin should be piped");
        writeln!(
            stdin,
            r#"{{"command_id":"cmd-play","command":"Play","tick":0}}"#
        )
        .unwrap();
        writeln!(stdin, r#"{{"tick":0}}"#).unwrap();
        writeln!(stdin, r#"{{"tick":40000000}}"#).unwrap();
        writeln!(
            stdin,
            r#"{{"command_id":"cmd-stop","command":"Stop","tick":40000000}}"#
        )
        .unwrap();
    }

    let output = child.wait_with_output().expect("runner should finish");
    assert!(
        output.status.success(),
        "runner failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout_contains_event(&stdout, "CommandAccepted", None));
    assert!(stdout_contains_event(&stdout, "FramePresented", Some(1)));
    assert_eq!(file_count(&output_dir.join("video"), "rgb"), 2);
    assert_eq!(file_count(&output_dir.join("audio"), "s16le"), 2);
}

#[test]
fn runner_accepts_stdin_jsonl_preload_and_active_source_switch() {
    require_runner_tools();
    let fixture = RealRunnerFixture::create_av();
    let output_dir = fixture.dir.join("jsonl-preload-out");
    let mut child = spawn_runner_jsonl_with_registered_source(&fixture, &output_dir, "src-b");

    {
        let stdin = child.stdin.as_mut().expect("runner stdin should be piped");
        writeln!(
            stdin,
            "{}",
            json!({
                "command_id": "cmd-preload",
                "command": {
                    "PreloadSource": {
                        "source": source_runtime_json("src-b")
                    }
                },
                "tick": 0
            })
        )
        .unwrap();
        writeln!(
            stdin,
            "{}",
            json!({
                "command_id": "cmd-active",
                "command": {
                    "SetActiveSource": {
                        "source_id": "src-b"
                    }
                },
                "tick": 0
            })
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"command_id":"cmd-play-next","command":"Play","tick":0}}"#
        )
        .unwrap();
        writeln!(stdin, r#"{{"tick":0}}"#).unwrap();
    }

    let output = child.wait_with_output().expect("runner should finish");
    assert!(
        output.status.success(),
        "runner failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout_contains_source_event(
        &stdout,
        "SourcePreloaded",
        "src-b"
    ));
    assert!(stdout_contains_source_event(
        &stdout,
        "SourceReady",
        "src-b"
    ));
    assert!(stdout_contains_event(&stdout, "FramePresented", Some(0)));
}

#[test]
fn runner_decodes_real_pcm_audio_only_file() {
    assert_runner_decodes_audio_fixture(RealRunnerFixture::create_pcm_audio, "50", "25/1", None);
}

#[test]
fn runner_decodes_real_encoded_audio_only_file() {
    assert_runner_decodes_audio_fixture(
        RealRunnerFixture::create_encoded_audio,
        "50",
        "25/1",
        None,
    );
}

#[test]
fn runner_decodes_real_audio_with_2997_frame_aligned_packets() {
    assert_runner_decodes_audio_fixture(
        RealRunnerFixture::create_pcm_audio,
        "120",
        "30000/1001",
        Some(&[3202, 3204, 3202, 3204, 3204]),
    );
}

#[test]
fn runner_decodes_real_audio_with_30_frame_aligned_packets() {
    assert_runner_decodes_audio_fixture(
        RealRunnerFixture::create_pcm_audio,
        "120",
        "30/1",
        Some(&[3200, 3200, 3200, 3200, 3200]),
    );
}

#[test]
fn runner_decodes_real_audio_with_5994_frame_aligned_packets() {
    assert_runner_decodes_audio_fixture(
        RealRunnerFixture::create_pcm_audio,
        "120",
        "60000/1001",
        Some(&[1600, 1602, 1602, 1602, 1602]),
    );
}

#[test]
fn runner_decodes_real_audio_with_60_frame_aligned_packets() {
    assert_runner_decodes_audio_fixture(
        RealRunnerFixture::create_pcm_audio,
        "120",
        "60/1",
        Some(&[1600, 1600, 1600, 1600, 1600]),
    );
}

fn assert_runner_decodes_av_fixture(create_fixture: fn() -> RealRunnerFixture) {
    require_runner_tools();
    let fixture = create_fixture();
    let output_dir = fixture.dir.join("out");
    let stdout = run_runner(
        &fixture,
        &output_dir,
        RunnerSmokeArgs {
            duration_frames: "50",
            timebase: "25/1",
            video_format: Some("160x90"),
            audio_format: Some("48000x1"),
            out_frame: Some("4"),
            max_ticks: "8",
        },
    );

    assert!(stdout_contains_event(&stdout, "FramePresented", Some(4)));
    assert!(stdout_contains_event(
        &stdout,
        "PlaybackBoundaryReached",
        Some(4)
    ));
    assert_eq!(file_count(&output_dir.join("video"), "rgb"), 5);
    assert_eq!(file_count(&output_dir.join("audio"), "s16le"), 5);
}

fn assert_runner_decodes_audio_fixture(
    create_fixture: fn() -> RealRunnerFixture,
    duration_frames: &'static str,
    timebase: &'static str,
    expected_packet_sizes: Option<&[u64]>,
) {
    require_runner_tools();
    let fixture = create_fixture();
    let output_dir = fixture.dir.join("out");
    let stdout = run_runner(
        &fixture,
        &output_dir,
        RunnerSmokeArgs {
            duration_frames,
            timebase,
            video_format: None,
            audio_format: Some("48000x1"),
            out_frame: Some("4"),
            max_ticks: "8",
        },
    );

    assert!(stdout_contains_event(&stdout, "AudioLevelChanged", None));
    assert!(stdout_contains_event(
        &stdout,
        "PlaybackBoundaryReached",
        Some(4)
    ));
    assert!(!stdout_contains_event(&stdout, "FramePresented", None));
    assert_eq!(file_count(&output_dir.join("audio"), "s16le"), 5);
    assert_eq!(file_count(&output_dir.join("video"), "rgb"), 0);
    if let Some(expected_packet_sizes) = expected_packet_sizes {
        assert_audio_packet_sizes(&output_dir.join("audio"), expected_packet_sizes);
    }
}

struct RunnerSmokeArgs {
    duration_frames: &'static str,
    timebase: &'static str,
    video_format: Option<&'static str>,
    audio_format: Option<&'static str>,
    out_frame: Option<&'static str>,
    max_ticks: &'static str,
}

fn run_runner(fixture: &RealRunnerFixture, output_dir: &Path, args: RunnerSmokeArgs) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_qnc-player-runner"));
    command.args([
        "--path",
        fixture.path.to_str().unwrap(),
        "--duration-frames",
        args.duration_frames,
        "--timebase",
        args.timebase,
        "--max-ticks",
        args.max_ticks,
        "--output-dir",
        output_dir.to_str().unwrap(),
        "--require-boundary",
    ]);
    if let Some(out_frame) = args.out_frame {
        command.args(["--out-frame", out_frame]);
    }
    if let Some(video_format) = args.video_format {
        command.args(["--video-format", video_format]);
    }
    if let Some(audio_format) = args.audio_format {
        command.args(["--audio-format", audio_format]);
    }

    let output = command.output().expect("runner should execute");
    assert!(
        output.status.success(),
        "runner failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout_contains_final_state(&stdout, true),
        "runner should emit final boundary state\nstdout:\n{stdout}"
    );
    stdout
}

fn run_runner_with_probe(fixture: &RealRunnerFixture, output_dir: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qnc-player-runner"))
        .args([
            "--path",
            fixture.path.to_str().unwrap(),
            "--probe-source-runtime",
            "--out-frame",
            "4",
            "--max-ticks",
            "8",
            "--output-dir",
            output_dir.to_str().unwrap(),
            "--require-boundary",
        ])
        .output()
        .expect("runner should execute");
    assert!(
        output.status.success(),
        "runner failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout_contains_final_state(&stdout, true),
        "runner should emit final boundary state\nstdout:\n{stdout}"
    );
    stdout
}

fn run_runner_with_monitor(fixture: &RealRunnerFixture, output_dir: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qnc-player-runner"))
        .args([
            "--path",
            fixture.path.to_str().unwrap(),
            "--duration-frames",
            "50",
            "--timebase",
            "25/1",
            "--out-frame",
            "4",
            "--max-ticks",
            "8",
            "--output-dir",
            output_dir.to_str().unwrap(),
            "--video-format",
            "160x90",
            "--audio-format",
            "48000x1",
            "--emit-monitor-state",
            "--require-boundary",
        ])
        .output()
        .expect("runner should execute");
    assert!(
        output.status.success(),
        "runner failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout_contains_final_state(&stdout, true),
        "runner should emit final boundary state\nstdout:\n{stdout}"
    );
    stdout
}

fn run_existing_source_with_probe(path: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qnc-player-runner"))
        .args([
            "--path",
            path.to_str().unwrap(),
            "--probe-source-runtime",
            "--out-frame",
            "1",
            "--max-ticks",
            "4",
            "--require-boundary",
        ])
        .output()
        .expect("runner should execute");
    assert!(
        output.status.success(),
        "runner failed for {}\nstdout:\n{}\nstderr:\n{}",
        path.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout_contains_final_state(&stdout, true),
        "runner should emit final boundary state for {}\nstdout:\n{stdout}",
        path.display()
    );
    stdout
}

fn run_runner_realtime_boundary(fixture: &RealRunnerFixture, output_dir: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qnc-player-runner"))
        .args([
            "--path",
            fixture.path.to_str().unwrap(),
            "--duration-frames",
            "50",
            "--timebase",
            "25/1",
            "--out-frame",
            "1",
            "--max-ticks",
            "4",
            "--output-dir",
            output_dir.to_str().unwrap(),
            "--video-format",
            "160x90",
            "--audio-format",
            "48000x1",
            "--realtime",
            "--require-boundary",
        ])
        .output()
        .expect("runner should execute");
    assert!(
        output.status.success(),
        "runner failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout_contains_final_state(&stdout, true),
        "runner should emit final boundary state\nstdout:\n{stdout}"
    );
    stdout
}

fn run_runner_realtime_diagnostics(fixture: &RealRunnerFixture, output_dir: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qnc-player-runner"))
        .args([
            "--path",
            fixture.path.to_str().unwrap(),
            "--duration-frames",
            "50",
            "--timebase",
            "25/1",
            "--out-frame",
            "49",
            "--max-ticks",
            "4",
            "--output-dir",
            output_dir.to_str().unwrap(),
            "--video-format",
            "160x90",
            "--audio-format",
            "48000x1",
            "--realtime-diagnostics",
            "--diagnostic-frames",
            "2",
            "--diagnostic-seek-frame",
            "10",
            "--require-boundary",
        ])
        .output()
        .expect("runner should execute");
    assert!(
        output.status.success(),
        "runner failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn spawn_runner_jsonl(fixture: &RealRunnerFixture, output_dir: &Path) -> Child {
    spawn_runner_jsonl_with_extra_source(fixture, output_dir, None)
}

fn spawn_runner_jsonl_with_registered_source(
    fixture: &RealRunnerFixture,
    output_dir: &Path,
    source_id: &str,
) -> Child {
    spawn_runner_jsonl_with_extra_source(fixture, output_dir, Some(source_id))
}

fn spawn_runner_jsonl_with_extra_source(
    fixture: &RealRunnerFixture,
    output_dir: &Path,
    extra_source_id: Option<&str>,
) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_qnc-player-runner"));
    command.args([
        "--path",
        fixture.path.to_str().unwrap(),
        "--duration-frames",
        "50",
        "--timebase",
        "25/1",
        "--out-frame",
        "4",
        "--output-dir",
        output_dir.to_str().unwrap(),
        "--video-format",
        "160x90",
        "--audio-format",
        "48000x1",
        "--stdin-jsonl",
    ]);
    if let Some(source_id) = extra_source_id {
        command.args([
            "--register-source",
            &format!("{source_id}={}", fixture.path.display()),
        ]);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("runner should spawn")
}

fn stdout_contains_event(stdout: &str, event_name: &str, frame: Option<u64>) -> bool {
    stdout.lines().any(|line| {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        let event = &value["event"];
        match frame {
            Some(frame) => event[event_name]["frame"] == frame,
            None => event.get(event_name).is_some(),
        }
    })
}

fn stdout_contains_source_event(stdout: &str, event_name: &str, source_id: &str) -> bool {
    stdout.lines().any(|line| {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        value["event"][event_name]["source_id"] == source_id
    })
}

fn stdout_contains_final_state(stdout: &str, reached_boundary: bool) -> bool {
    stdout.lines().any(|line| {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        value["stage"] == "final_state"
            && value["reached_boundary"] == reached_boundary
            && value["state"]["carrier_frame"].is_u64()
    })
}

fn stdout_contains_monitor_state(stdout: &str, frame: u64, byte_len: u64) -> bool {
    stdout.lines().any(|line| {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        let state = &value["state"];
        value["stage"] == "monitor_state"
            && state["carrier_frame"] == frame
            && state["presented_frame"] == frame
            && state["boundary_frame"] == frame
            && state["last_frame_buffer"]["frame"] == frame
            && state["last_frame_buffer"]["byte_len"] == byte_len
            && state["last_error"].is_null()
    })
}

fn stdout_contains_realtime_diagnostics(stdout: &str, seek_frame: u64) -> bool {
    stdout.lines().any(|line| {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        let diagnostics = &value["diagnostics"];
        value["stage"] == "realtime_diagnostics"
            && diagnostics["seek_frame"] == seek_frame
            && diagnostics["reached_boundary"] == true
            && diagnostics["command_records"]
                .as_array()
                .is_some_and(|records| {
                    records
                        .iter()
                        .any(|record| record["command_name"] == "SetPlaybackRequest")
                })
            && diagnostics["segment_records"]
                .as_array()
                .is_some_and(|records| records.iter().any(|record| record["label"] == "seek_play"))
            && diagnostics["segment_records"]
                .as_array()
                .is_some_and(|records| records.iter().all(segment_has_one_av_output_per_tick))
            && diagnostics["totals"]["playback_dropped_frame_count"] == 0
            && diagnostics["totals"]["playback_av_sync_warning_count"] == 0
            && diagnostics["totals"]["playback_error_count"] == 0
    })
}

fn segment_has_one_av_output_per_tick(record: &Value) -> bool {
    record["frame_presented_count"] == record["tick_count"]
        && record["audio_level_event_count"] == record["tick_count"]
}

fn source_runtime_json(source_id: &str) -> Value {
    json!({
        "source_id": source_id,
        "duration_frames": 50,
        "timebase": {
            "frame_rate_num": 25,
            "frame_rate_den": 1
        },
        "source_start_tc": null,
        "video_format": {
            "width": 160,
            "height": 90,
            "field_mode": "Progressive",
            "color_space": "Rec709",
            "pixel_aspect": {
                "num": 1,
                "den": 1
            }
        },
        "audio_format": {
            "sample_rate_hz": 48000,
            "channel_count": 1
        }
    })
}

fn assert_audio_packet_sizes(dir: &Path, expected_sizes: &[u64]) {
    assert_eq!(file_count(dir, "s16le"), expected_sizes.len());
    for (frame, expected_size) in expected_sizes.iter().enumerate() {
        let path = dir.join(format!("audio-frame-{frame:08}.s16le"));
        let actual_size = std::fs::metadata(&path)
            .unwrap_or_else(|err| panic!("should read {}: {err}", path.display()))
            .len();
        assert_eq!(
            actual_size,
            *expected_size,
            "wrong audio packet byte size for {}",
            path.display()
        );
    }
}

fn file_count(dir: &Path, extension: &str) -> usize {
    if !dir.exists() {
        return 0;
    }
    std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("should read {}: {err}", dir.display()))
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.path().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|actual| actual == extension)
        })
        .count()
}

fn collect_media_files(root: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_media_files_into(root, extensions, &mut paths);
    paths.sort();
    paths
}

fn collect_media_files_into(root: &Path, extensions: &[&str], paths: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(root)
        .unwrap_or_else(|err| panic!("should read {}: {err}", root.display()));
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_media_files_into(&path, extensions, paths);
        } else if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|ext| {
                extensions
                    .iter()
                    .any(|expected| ext.eq_ignore_ascii_case(expected))
            })
        {
            paths.push(path);
        }
    }
}

fn require_runner_tools() {
    require_tool(&ffmpeg_tool(), "ffmpeg");
    require_tool(&ffprobe_tool(), "ffprobe");
}

fn require_tool(tool: &Path, label: &str) {
    let output = Command::new(tool)
        .arg("-version")
        .output()
        .unwrap_or_else(|err| {
            panic!(
                "{label} is required for real runner smoke test at {}: {err}",
                tool.display()
            )
        });
    assert!(
        output.status.success(),
        "{label} is required for real runner smoke test at {}",
        tool.display()
    );
}

fn ffmpeg_tool() -> PathBuf {
    std::env::var_os("QNC_FFMPEG")
        .filter(|value| !value.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

fn ffprobe_tool() -> PathBuf {
    std::env::var_os("QNC_FFPROBE")
        .filter(|value| !value.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ffprobe"))
}

struct RealRunnerFixture {
    path: PathBuf,
    dir: PathBuf,
}

impl RealRunnerFixture {
    fn create_av() -> Self {
        Self::create_with(
            "runner-smoke.mp4",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=25:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=48000:duration=2",
                "-frames:v",
                "50",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ],
        )
    }

    fn create_long_av() -> Self {
        Self::create_with(
            "runner-long-smoke.mp4",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=25:duration=10",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=48000:duration=10",
                "-frames:v",
                "250",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ],
        )
    }

    fn create_mov_av() -> Self {
        Self::create_with(
            "runner-smoke.mov",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=25:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=48000:duration=2",
                "-frames:v",
                "50",
                "-c:v",
                "prores_ks",
                "-profile:v",
                "0",
                "-pix_fmt",
                "yuv422p10le",
                "-c:a",
                "pcm_s16le",
                "-ar",
                "48000",
                "-ac",
                "1",
                "-shortest",
            ],
        )
    }

    fn create_mxf_av() -> Self {
        Self::create_with(
            "runner-smoke.mxf",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=25:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=48000:duration=2",
                "-frames:v",
                "50",
                "-c:v",
                "mpeg2video",
                "-q:v",
                "2",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "pcm_s16le",
                "-ar",
                "48000",
                "-ac",
                "1",
                "-shortest",
                "-f",
                "mxf",
            ],
        )
    }

    fn create_mpeg_ts_av() -> Self {
        Self::create_with(
            "runner-smoke.m2ts",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=25:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=48000:duration=2",
                "-frames:v",
                "50",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
                "-f",
                "mpegts",
            ],
        )
    }

    fn create_pcm_audio() -> Self {
        Self::create_with(
            "runner-audio.wav",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=48000:duration=2",
                "-c:a",
                "pcm_s16le",
            ],
        )
    }

    fn create_encoded_audio() -> Self {
        Self::create_with(
            "runner-audio.mp3",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=48000:duration=2",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "128k",
            ],
        )
    }

    fn create_with(file_name: &str, ffmpeg_args: &[&str]) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "qnc-real-runner-smoke-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(file_name);
        let output = Command::new(ffmpeg_tool())
            .args(ffmpeg_args)
            .arg(&path)
            .output()
            .expect("ffmpeg should create runner fixture");
        assert!(
            output.status.success(),
            "ffmpeg fixture creation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self { path, dir }
    }
}

impl Drop for RealRunnerFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
