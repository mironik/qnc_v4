use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::{convert::Infallible, sync::mpsc as std_mpsc};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

use crate::app_state::AppState;
use crate::media_pool::{get_transcript, proxy_path_for_clip, save_transcript};
use crate::project::db::{project_settings_snapshot, ProjectPaths};

static ASR_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static TRANSLATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug)]
struct AsrRuntime {
    enabled: bool,
    cli: Option<PathBuf>,
    model: Option<PathBuf>,
    vad_model: Option<PathBuf>,
    translator_cli: Option<PathBuf>,
    translator_model: String,
    translator_ready: bool,
    ffmpeg: Option<PathBuf>,
    timeout: Duration,
}

#[derive(serde::Deserialize)]
struct TranscribeBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_ids: Vec<String>,
    #[serde(default)]
    language: String,
    start_sec: Option<f64>,
    end_sec: Option<f64>,
}

#[derive(serde::Deserialize)]
struct TranslateBody {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    clip_id: String,
    start_sec: Option<f64>,
    end_sec: Option<f64>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/asr/health", get(api_asr_health))
        .route("/api/translation/health", get(api_translation_health))
        .route(
            "/api/ai-search/transcribe-stream",
            post(api_transcribe_stream),
        )
        .route(
            "/api/ai-search/translate-transcript",
            post(api_translate_transcript),
        )
}

pub fn available(root: &Path) -> bool {
    AsrRuntime::resolve_asr(root).ready()
}

pub fn capability(root: &Path) -> Value {
    AsrRuntime::resolve(root).as_json()
}

impl AsrRuntime {
    fn resolve(root: &Path) -> Self {
        Self::resolve_inner(root, true)
    }

    fn resolve_asr(root: &Path) -> Self {
        Self::resolve_inner(root, false)
    }

    fn resolve_inner(root: &Path, check_translation: bool) -> Self {
        let cli = std::env::var("QNC_WHISPER_CLI")
            .ok()
            .and_then(|raw| resolve_command(raw.trim()))
            .or_else(|| resolve_command("whisper-cli"))
            .or_else(|| discover_cli(root));
        let model = std::env::var("QNC_WHISPER_MODEL")
            .ok()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| discover_model(root));
        let vad_model = std::env::var("QNC_WHISPER_VAD_MODEL")
            .ok()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| discover_vad_model(root));
        let translator_cli = std::env::var("QNC_TRANSLATE_CLI")
            .ok()
            .and_then(|raw| resolve_command(raw.trim()))
            .or_else(|| resolve_command("ollama"))
            .or_else(|| discover_ollama_cli());
        let translator_model = std::env::var("QNC_TRANSLATE_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "translategemma:4b".into());
        let translator_ready = check_translation
            && translator_cli
                .as_deref()
                .map(|cli| translator_model_available(cli, &translator_model))
                .unwrap_or(false);
        let ffmpeg = crate::ingest::thumb::resolve_ffmpeg();
        let configured = cli.is_some() && model.is_some();
        let enabled = env_flag("QNC_AI_ENABLED").unwrap_or(configured);
        let timeout_secs = std::env::var("QNC_ASR_TIMEOUT_SEC")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(3600)
            .clamp(30, 86_400);
        Self {
            enabled,
            cli,
            model,
            vad_model,
            translator_cli,
            translator_model,
            translator_ready,
            ffmpeg,
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    fn ready(&self) -> bool {
        self.enabled && self.cli.is_some() && self.model.is_some() && self.ffmpeg.is_some()
    }

    fn translation_ready(&self) -> bool {
        self.translator_cli.is_some() && self.translator_ready
    }

    fn message(&self) -> String {
        if !self.enabled {
            return "ASR je isključen. Postavi QNC_AI_ENABLED=1.".into();
        }
        if self.cli.is_none() {
            return "whisper-cli nije pronađen. Postavi QNC_WHISPER_CLI.".into();
        }
        if self.model.is_none() {
            return "Whisper model nije pronađen. Postavi QNC_WHISPER_MODEL.".into();
        }
        if self.ffmpeg.is_none() {
            return "FFmpeg nije pronađen; ASR ne može pripremiti 16 kHz WAV.".into();
        }
        "Lokalni ASR je spreman. Prijevod se pokreće zasebnom naredbom.".into()
    }

    fn as_json(&self) -> Value {
        json!({
            "status": if self.ready() { "ok" } else { "offline" },
            "backend": "whisper_cpp_cli",
            "enabled": self.enabled,
            "ready": self.ready(),
            "message": self.message(),
            "model": self.model.as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default(),
            "vad": self.vad_model.is_some(),
            "vad_model": self.vad_model.as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default(),
            "translation_ready": self.translation_ready(),
            "translation_backend": "ollama",
            "translation_model": self.translator_model,
            "system_language": system_language(),
            "cli_configured": self.cli.is_some(),
            "model_configured": self.model.is_some(),
            "ffmpeg": self.ffmpeg.is_some(),
        })
    }
}

fn discover_cli(root: &Path) -> Option<PathBuf> {
    let executable = if cfg!(windows) {
        "whisper-cli.exe"
    } else {
        "whisper-cli"
    };
    [
        root.join("tools").join("whisper").join(executable),
        root.join("tools")
            .join("whisper")
            .join("bin")
            .join(executable),
        root.join("tools")
            .join("whisper")
            .join("build")
            .join("bin")
            .join("Release")
            .join(executable),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

async fn api_asr_health(State(app): State<AppState>) -> Json<Value> {
    Json(AsrRuntime::resolve_asr(&app.root).as_json())
}

async fn api_translation_health(State(app): State<AppState>) -> Json<Value> {
    let runtime = AsrRuntime::resolve(&app.root);
    let ready = runtime.translation_ready();
    Json(json!({
        "status": if ready { "ok" } else { "offline" },
        "ready": ready,
        "backend": "ollama",
        "model": runtime.translator_model,
        "system_language": system_language(),
        "message": if ready {
            format!(
                "Lokalni prijevod na jezik sustava ({}) je spreman.",
                system_language()
            )
        } else if runtime.translator_cli.is_none() {
            "Ollama nije pronađen; lokalni prijevod nije dostupan.".to_string()
        } else {
            format!(
                "Ollama model {} nije instaliran; pokreni scripts/setup-translation.ps1.",
                runtime.translator_model
            )
        },
    }))
}

async fn api_transcribe_stream(
    State(app): State<AppState>,
    Json(body): Json<TranscribeBody>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let project_id = if body.project_id.trim().is_empty() {
        app.project
            .active_project_id()
            .map_err(|(_, message)| api_error(StatusCode::BAD_REQUEST, message))?
    } else {
        body.project_id.trim().to_string()
    };
    if !project_ai_enabled(&app.project.paths, &project_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "AI/transkripcija nije uključena u postavkama projekta.",
        ));
    }

    let clip_ids: Vec<String> = body
        .clip_ids
        .iter()
        .map(|clip_id| clip_id.trim().to_string())
        .filter(|clip_id| !clip_id.is_empty())
        .collect();
    if clip_ids.len() != 1 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "ASR stream prima točno jedan clip_id po zahtjevu.",
        ));
    }
    let clip_id = clip_ids[0].clone();
    let media =
        proxy_path_for_clip(&app.project.paths, &project_id, &clip_id).ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                format!("Proxy medij nije pronađen za '{clip_id}'."),
            )
        })?;

    let runtime = AsrRuntime::resolve_asr(&app.root);
    if !runtime.ready() {
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            runtime.message(),
        ));
    }

    let mut language = normalize_language(&body.language);
    let range = transcription_range(body.start_sec, body.end_sec)
        .map_err(|message| api_error(StatusCode::BAD_REQUEST, message))?;
    let paths = app.project.paths.clone();
    let pid = project_id.clone();
    let cid = clip_id.clone();
    let previous_transcript = get_transcript(&paths, &pid, &cid).ok().flatten();
    if language == "auto" && range.is_some() {
        language = previous_transcript
            .as_ref()
            .and_then(|transcript| {
                transcript
                    .get("source_language")
                    .or_else(|| transcript.get("language"))
            })
            .and_then(Value::as_str)
            .map(normalize_language)
            .filter(|language| language != "auto")
            .unwrap_or(language);
    }
    let processing_transcript = previous_transcript.clone().unwrap_or_else(|| {
        json!({
            "text": "",
            "segments": [],
            "scope": range.map(|(start, end)| json!({ "start": start, "end": end })),
        })
    });
    let _ = save_transcript(&paths, &pid, &cid, "processing", &processing_transcript);

    let (event_tx, event_rx) = mpsc::channel::<String>(64);
    let stream_clip_id = clip_id.clone();
    tokio::spawn(async move {
        let worker_tx = event_tx.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let mut on_segment = |segment: Value| {
                let _ = worker_tx.blocking_send(sse_event(json!({
                    "type": "segment",
                    "clip_id": stream_clip_id,
                    "segment": segment,
                })));
            };
            let result =
                transcribe_with_whisper(&runtime, &media, &language, range, &mut on_segment);
            match result {
                Ok(transcript) => {
                    if let Err(error) = save_transcript(&paths, &pid, &cid, "complete", &transcript)
                    {
                        let _ = worker_tx.blocking_send(sse_event(json!({
                            "type": "error",
                            "clip_id": cid,
                            "error": error,
                        })));
                        return;
                    }
                    let _ = worker_tx.blocking_send(sse_event(json!({
                        "type": "complete",
                        "clip_id": cid,
                        "transcript": transcript,
                    })));
                }
                Err(error) => {
                    if let Some(previous) = previous_transcript {
                        let _ = save_transcript(&paths, &pid, &cid, "complete", &previous);
                    } else {
                        let _ = save_transcript(
                            &paths,
                            &pid,
                            &cid,
                            "failed",
                            &json!({ "text": "", "segments": [], "error": error }),
                        );
                    }
                    let _ = worker_tx.blocking_send(sse_event(json!({
                        "type": "error",
                        "clip_id": cid,
                        "error": error,
                    })));
                }
            }
        })
        .await;
        if let Err(error) = worker {
            let _ = event_tx
                .send(sse_event(json!({
                    "type": "error",
                    "clip_id": clip_id,
                    "error": format!("ASR worker: {error}"),
                })))
                .await;
        }
    });
    Ok(sse_stream_response(event_rx))
}

async fn api_translate_transcript(
    State(app): State<AppState>,
    Json(body): Json<TranslateBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let project_id = if body.project_id.trim().is_empty() {
        app.project
            .active_project_id()
            .map_err(|(_, message)| api_error(StatusCode::BAD_REQUEST, message))?
    } else {
        body.project_id.trim().to_string()
    };
    if !project_ai_enabled(&app.project.paths, &project_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "AI/prijevod nije uključen u postavkama projekta.",
        ));
    }
    let clip_id = body.clip_id.trim().to_string();
    if clip_id.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "Nedostaje clip_id."));
    }
    let range = transcription_range(body.start_sec, body.end_sec)
        .map_err(|message| api_error(StatusCode::BAD_REQUEST, message))?;
    let transcript = get_transcript(&app.project.paths, &project_id, &clip_id)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                format!("Transkript za '{clip_id}' nije pronađen u bazi."),
            )
        })?;
    let runtime = AsrRuntime::resolve(&app.root);
    if !runtime.translation_ready() {
        let message = if runtime.translator_cli.is_none() {
            "Ollama nije pronađen; lokalni prijevod nije dostupan.".to_string()
        } else {
            format!(
                "Ollama model {} nije instaliran; pokreni scripts/setup-translation.ps1.",
                runtime.translator_model
            )
        };
        return Err(api_error(StatusCode::SERVICE_UNAVAILABLE, message));
    }

    let target_language = system_language();
    let result = tokio::task::spawn_blocking(move || {
        translate_saved_transcript(&runtime, transcript, &target_language, range)
    })
    .await
    .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
    .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;

    save_transcript(
        &app.project.paths,
        &project_id,
        &clip_id,
        "complete",
        &result,
    )
    .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(json!({
        "status": "ok",
        "project_id": project_id,
        "clip_id": clip_id,
        "transcript": result,
    })))
}

fn project_ai_enabled(paths: &ProjectPaths, project_id: &str) -> bool {
    let Ok(snapshot) = project_settings_snapshot(paths, project_id) else {
        return false;
    };
    let ai = snapshot
        .get("settings")
        .and_then(|settings| settings.get("ai"));
    ai.and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || ai
            .and_then(|value| value.get("transcription_enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn transcription_range(
    start_sec: Option<f64>,
    end_sec: Option<f64>,
) -> Result<Option<(f64, f64)>, String> {
    match (start_sec, end_sec) {
        (None, None) => Ok(None),
        (Some(start), Some(end))
            if start.is_finite() && end.is_finite() && start >= 0.0 && end - start >= 0.04 =>
        {
            Ok(Some((round3(start), round3(end))))
        }
        (Some(_), Some(_)) => Err("ASR IN–OUT raspon nije valjan (OUT mora biti nakon IN).".into()),
        _ => Err("ASR IN–OUT zahtijeva i start_sec i end_sec.".into()),
    }
}

fn transcribe_with_whisper(
    runtime: &AsrRuntime,
    media: &Path,
    language: &str,
    range: Option<(f64, f64)>,
    on_segment: &mut dyn FnMut(Value),
) -> Result<Value, String> {
    let _guard = ASR_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|error| format!("ASR worker lock: {error}"))?;
    let temp_dir = std::env::temp_dir().join(format!("qnc-asr-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&temp_dir).map_err(|error| error.to_string())?;
    let result = transcribe_in_temp(runtime, media, language, range, &temp_dir, on_segment);
    let _ = fs::remove_dir_all(&temp_dir);
    result
}

fn transcribe_in_temp(
    runtime: &AsrRuntime,
    media: &Path,
    language: &str,
    range: Option<(f64, f64)>,
    temp_dir: &Path,
    on_segment: &mut dyn FnMut(Value),
) -> Result<Value, String> {
    let wav = temp_dir.join("audio.wav");
    let ffmpeg_log = temp_dir.join("ffmpeg.log");
    let mut ffmpeg = Command::new(runtime.ffmpeg.as_ref().ok_or("FFmpeg nije konfiguriran")?);
    ffmpeg.args(["-y", "-v", "error", "-i"]).arg(media);
    if let Some((start, end)) = range {
        ffmpeg.args([
            "-ss",
            &format!("{start:.3}"),
            "-t",
            &format!("{:.3}", end - start),
        ]);
    }
    ffmpeg
        .args([
            "-vn",
            "-af",
            "highpass=f=70,lowpass=f=8000,loudnorm=I=-16:LRA=11:TP=-1.5",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(&wav);
    run_command(
        &mut ffmpeg,
        &ffmpeg_log,
        runtime.timeout.min(Duration::from_secs(900)),
        "FFmpeg audio priprema",
    )?;
    if !wav.is_file() {
        return Err("FFmpeg nije proizveo ASR WAV datoteku.".into());
    }

    let output_prefix = temp_dir.join("transcript");
    let whisper_log = temp_dir.join("whisper.log");
    let mut whisper = Command::new(
        runtime
            .cli
            .as_ref()
            .ok_or("whisper-cli nije konfiguriran")?,
    );
    whisper
        .args(["-m"])
        .arg(
            runtime
                .model
                .as_ref()
                .ok_or("Whisper model nije konfiguriran")?,
        )
        .args(["-f"])
        .arg(&wav)
        .args(["-l", language, "-oj", "-of"])
        .arg(&output_prefix);
    whisper.args(["-mc", "0", "-sns", "-nf", "-lpt", "-0.8", "-nth", "0.65"]);
    if let Some(vad_model) = runtime.vad_model.as_ref() {
        whisper.arg("--vad").args(["-vm"]).arg(vad_model).args([
            "-vt", "0.50", "-vspd", "250", "-vsd", "500", "-vmsd", "30", "-vp", "200", "-vo",
            "0.25",
        ]);
    }
    let threads = std::env::var("QNC_WHISPER_THREADS")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|threads| *threads > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|count| count.get() as u32)
                .unwrap_or(4)
                .clamp(1, 8)
        });
    whisper.args(["-t", &threads.to_string()]);
    if env_flag("QNC_WHISPER_NO_GPU") == Some(true) {
        whisper.arg("-ng");
    }
    run_whisper_command(
        &mut whisper,
        &whisper_log,
        runtime.timeout,
        range.map(|(start, _)| start).unwrap_or(0.0),
        on_segment,
    )?;

    let json_path = output_prefix.with_extension("json");
    let raw = fs::read_to_string(&json_path).map_err(|error| {
        format!(
            "Whisper JSON nije pronađen ({}): {error}",
            json_path.display()
        )
    })?;
    let doc: Value =
        serde_json::from_str(&raw).map_err(|error| format!("Neispravan Whisper JSON: {error}"))?;
    let mut transcript = parse_whisper_json(&doc)?;
    if let Some((start, end)) = range {
        shift_transcript_timestamps(&mut transcript, start);
        if let Some(object) = transcript.as_object_mut() {
            object.insert("scope".into(), json!({ "start": start, "end": end }));
        }
    }
    if let Some(object) = transcript.as_object_mut() {
        let detected_language = object
            .get("language")
            .and_then(Value::as_str)
            .map(normalize_language)
            .unwrap_or_else(|| "auto".into());
        object.insert("source_language".into(), json!(detected_language));
        object.insert("translated".into(), json!(false));
    }
    Ok(transcript)
}

fn run_command(
    command: &mut Command,
    log_path: &Path,
    timeout: Duration,
    label: &str,
) -> Result<(), String> {
    let stdout = File::create(log_path).map_err(|error| format!("{label}: {error}"))?;
    let stderr = stdout
        .try_clone()
        .map_err(|error| format!("{label}: {error}"))?;
    let mut child = command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| format!("{label}: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let log = fs::read_to_string(log_path).unwrap_or_default();
                return Err(format!(
                    "{label} nije uspio ({status}): {}",
                    tail(&log, 3000)
                ));
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{label} je prekinut nakon {} s.",
                    timeout.as_secs()
                ));
            }
            Err(error) => return Err(format!("{label}: {error}")),
        }
    }
}

fn run_whisper_command(
    command: &mut Command,
    log_path: &Path,
    timeout: Duration,
    timestamp_offset: f64,
    on_segment: &mut dyn FnMut(Value),
) -> Result<(), String> {
    let stderr_path = log_path.with_extension("stderr.log");
    let stderr = File::create(&stderr_path)
        .map_err(|error| format!("whisper.cpp transkripcija: {error}"))?;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| format!("whisper.cpp transkripcija: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("whisper.cpp transkripcija: stdout nije dostupan")?;
    let (line_tx, line_rx) = std_mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut log =
        File::create(log_path).map_err(|error| format!("whisper.cpp transkripcija: {error}"))?;
    let mut previous = String::new();
    let started = Instant::now();
    loop {
        while let Ok(line) = line_rx.try_recv() {
            let _ = writeln!(log, "{line}");
            emit_realtime_segment(&line, timestamp_offset, &mut previous, on_segment);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = reader.join();
                while let Ok(line) = line_rx.try_recv() {
                    let _ = writeln!(log, "{line}");
                    emit_realtime_segment(&line, timestamp_offset, &mut previous, on_segment);
                }
                if status.success() {
                    return Ok(());
                }
                let error = fs::read_to_string(&stderr_path).unwrap_or_default();
                return Err(format!(
                    "whisper.cpp transkripcija nije uspjela ({status}): {}",
                    tail(&error, 3000)
                ));
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(30));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(format!(
                    "whisper.cpp transkripcija je prekinuta nakon {} s.",
                    timeout.as_secs()
                ));
            }
            Err(error) => return Err(format!("whisper.cpp transkripcija: {error}")),
        }
    }
}

fn emit_realtime_segment(
    line: &str,
    timestamp_offset: f64,
    previous: &mut String,
    on_segment: &mut dyn FnMut(Value),
) {
    let Some(segment) = parse_realtime_segment(line, timestamp_offset) else {
        return;
    };
    let normalized = segment
        .get("text")
        .and_then(Value::as_str)
        .map(normalize_transcript_text)
        .unwrap_or_default();
    if normalized.is_empty() || normalized == *previous {
        return;
    }
    *previous = normalized;
    on_segment(segment);
}

fn parse_realtime_segment(line: &str, timestamp_offset: f64) -> Option<Value> {
    let clean = strip_ansi(line);
    let arrow = clean.find("-->")?;
    let open = clean[..arrow].rfind('[')?;
    let close = clean[arrow..].find(']')? + arrow;
    let start = parse_timestamp(clean[open + 1..arrow].trim())?;
    let end = parse_timestamp(clean[arrow + 3..close].trim())?;
    let text = clean[close + 1..].trim();
    if text.is_empty() {
        return None;
    }
    Some(json!({
        "start": round3(start + timestamp_offset),
        "end": round3(end.max(start) + timestamp_offset),
        "text": text,
    }))
}

fn parse_whisper_json(doc: &Value) -> Result<Value, String> {
    let entries = doc
        .get("transcription")
        .and_then(Value::as_array)
        .or_else(|| doc.get("segments").and_then(Value::as_array))
        .ok_or("Whisper JSON nema transcription/segments polje.")?;
    let mut segments: Vec<Value> = Vec::new();
    let mut previous = String::new();
    for entry in entries {
        if let Some(segment) = (|| {
            let text = entry.get("text").and_then(Value::as_str)?.trim();
            if text.is_empty() {
                return None;
            }
            let normalized = normalize_transcript_text(text);
            if normalized.is_empty() || normalized == previous {
                return None;
            }
            let (start, end) = segment_times(entry);
            previous = normalized;
            Some(json!({
                "start": round3(start),
                "end": round3(end.max(start)),
                "text": text,
            }))
        })() {
            segments.push(segment);
        }
    }
    let text = segments
        .iter()
        .filter_map(|segment| segment.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(json!({
        "text": text,
        "segments": segments,
        "language": doc.get("result")
            .and_then(|result| result.get("language"))
            .cloned()
            .unwrap_or(Value::Null),
    }))
}

fn normalize_transcript_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_lowercase().next().unwrap_or(character)
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn shift_transcript_timestamps(transcript: &mut Value, offset_sec: f64) {
    let Some(segments) = transcript.get_mut("segments").and_then(Value::as_array_mut) else {
        return;
    };
    for segment in segments {
        for field in ["start", "end"] {
            let shifted = segment
                .get(field)
                .and_then(Value::as_f64)
                .map(|value| round3(value + offset_sec));
            if let (Some(value), Some(object)) = (shifted, segment.as_object_mut()) {
                object.insert(field.into(), json!(value));
            }
        }
    }
}

fn translate_saved_transcript(
    runtime: &AsrRuntime,
    mut transcript: Value,
    target_language: &str,
    range: Option<(f64, f64)>,
) -> Result<Value, String> {
    let _guard = TRANSLATION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|error| format!("Translation worker lock: {error}"))?;
    let temp_dir =
        std::env::temp_dir().join(format!("qnc-translate-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&temp_dir).map_err(|error| error.to_string())?;
    let result =
        translate_transcript_in_temp(runtime, &mut transcript, target_language, range, &temp_dir);
    let _ = fs::remove_dir_all(&temp_dir);
    result.map(|_| transcript)
}

fn translate_transcript_in_temp(
    runtime: &AsrRuntime,
    transcript: &mut Value,
    target_language: &str,
    range: Option<(f64, f64)>,
    temp_dir: &Path,
) -> Result<(), String> {
    let source_language = transcript
        .get("source_language")
        .or_else(|| transcript.get("language"))
        .and_then(Value::as_str)
        .map(normalize_language)
        .unwrap_or_else(|| "auto".into());
    let target_language = normalize_language(target_language);
    if target_language == "auto" || target_language.is_empty() {
        return Err("Ciljni jezik sustava nije moguće odrediti.".into());
    }
    if source_language == target_language {
        if let Some(object) = transcript.as_object_mut() {
            object.insert("source_language".into(), json!(source_language));
            object.insert("language".into(), json!(target_language));
            object.insert("translated".into(), json!(false));
        }
        return Ok(());
    }
    let cli = runtime.translator_cli.as_ref().ok_or_else(|| {
        "Lokalni prevoditelj nije dostupan. Instaliraj Ollama i model translategemma:4b."
            .to_string()
    })?;
    let source_name = language_name(&source_language);
    let target_name = language_name(&target_language);
    let terminology = translation_terminology(&source_language, &target_language);
    let segments = transcript
        .get_mut("segments")
        .and_then(Value::as_array_mut)
        .ok_or("Transkript nema segmente za prijevod.")?;
    let mut translated_count = 0_usize;
    for (index, segment) in segments.iter_mut().enumerate() {
        let segment_start = segment.get("start").and_then(Value::as_f64).unwrap_or(0.0);
        let segment_end = segment
            .get("end")
            .and_then(Value::as_f64)
            .unwrap_or(segment_start);
        if let Some((range_start, range_end)) = range {
            if segment_end <= range_start || segment_start >= range_end {
                continue;
            }
        }
        let source_text = segment
            .get("source_text")
            .or_else(|| segment.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if source_text.is_empty() {
            continue;
        }
        let prompt = format!(
            "You are a professional {source_name} ({source_language}) to {target_name} ({target_language}) translator \
and {target_name} news editor. Translate accurately into natural, grammatically correct standard {target_name} \
suitable for a television news transcript. Preserve the meaning, correct obvious source-language grammatical slips \
from speech, and do not invent information. {terminology} Before answering, silently proofread spelling, grammar \
and meaning.\n\
Produce only the {target_name} translation, without any additional explanations or commentary. \
Please translate the following {source_name} text into {target_name}:\n\n{source_text}"
        );
        let stdout_path = temp_dir.join(format!("translate-{index}.out"));
        let stderr_path = temp_dir.join(format!("translate-{index}.err"));
        let mut command = Command::new(cli);
        command
            .args([
                "run",
                runtime.translator_model.as_str(),
                "--nowordwrap",
                "--keepalive",
                "10m",
            ])
            .arg(prompt);
        let translated = run_command_capture(
            &mut command,
            &stdout_path,
            &stderr_path,
            runtime.timeout.min(Duration::from_secs(300)),
            "Ollama prijevod",
        )?;
        let translated = clean_translation_output(&translated);
        if translated.is_empty() {
            return Err(format!("Prevoditelj je vratio prazan segment {index}."));
        }
        if let Some(object) = segment.as_object_mut() {
            object.insert("source_text".into(), json!(source_text));
            object.insert("text".into(), json!(translated));
            object.insert("source_language".into(), json!(source_language));
            object.insert("language".into(), json!(target_language));
            object.insert("translated".into(), json!(true));
        }
        translated_count += 1;
    }
    if translated_count == 0 {
        return Err(match range {
            Some(_) => "U odabranom IN–OUT rasponu nema segmenata za prijevod.".into(),
            None => "Transkript nema segmente za prijevod.".into(),
        });
    }
    let translated_text = segments
        .iter()
        .filter_map(|segment| segment.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let all_translated = segments.iter().all(|segment| {
        segment
            .get("translated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    if let Some(object) = transcript.as_object_mut() {
        if object.get("source_text").is_none() {
            let source_text = object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            object.insert("source_text".into(), json!(source_text));
        }
        object.insert("text".into(), json!(translated_text));
        object.insert("source_language".into(), json!(source_language));
        object.insert("language".into(), json!(target_language));
        object.insert("translated".into(), json!(all_translated));
        object.insert("partially_translated".into(), json!(!all_translated));
        object.insert(
            "translation_scope".into(),
            range
                .map(|(start, end)| json!({ "start": start, "end": end }))
                .unwrap_or_else(|| json!("full")),
        );
        object.insert("translation_model".into(), json!(runtime.translator_model));
    }
    Ok(())
}

fn run_command_capture(
    command: &mut Command,
    stdout_path: &Path,
    stderr_path: &Path,
    timeout: Duration,
    label: &str,
) -> Result<String, String> {
    let stdout = File::create(stdout_path).map_err(|error| format!("{label}: {error}"))?;
    let stderr = File::create(stderr_path).map_err(|error| format!("{label}: {error}"))?;
    let mut child = command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| format!("{label}: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return fs::read_to_string(stdout_path)
                    .map_err(|error| format!("{label}: {error}"));
            }
            Ok(Some(status)) => {
                let error = fs::read_to_string(stderr_path).unwrap_or_default();
                return Err(format!(
                    "{label} nije uspio ({status}): {}",
                    tail(&error, 3000)
                ));
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{label} je prekinut nakon {} s.",
                    timeout.as_secs()
                ));
            }
            Err(error) => return Err(format!("{label}: {error}")),
        }
    }
}

fn clean_translation_output(value: &str) -> String {
    let clean = strip_ansi(value);
    let trimmed = clean.trim();
    let without_thinking = trimmed
        .rsplit_once("</think>")
        .map(|(_, output)| output.trim())
        .unwrap_or(trimmed);
    without_thinking
        .trim_matches(|character| character == '"' || character == '“' || character == '”')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_ansi(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            output.push(character);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            let mut parameters = String::new();
            for control in chars.by_ref() {
                if ('@'..='~').contains(&control) {
                    if control == 'D' {
                        let count = parameters
                            .split(';')
                            .next()
                            .and_then(|raw| raw.parse::<usize>().ok())
                            .unwrap_or(1);
                        for _ in 0..count {
                            output.pop();
                        }
                    }
                    break;
                }
                parameters.push(control);
            }
        }
    }
    output
}

fn translation_terminology(source: &str, target: &str) -> &'static str {
    if source == "en" && target == "hr" {
        return "Use established Croatian terminology: \"lack of respect for international law\" means \
\"nepoštivanje međunarodnog prava\"; \"international order\" means \"međunarodni poredak\"; \
\"global governance\" means \"globalno upravljanje\".";
    }
    ""
}

fn language_name(code: &str) -> &'static str {
    match code {
        "hr" => "Croatian",
        "en" => "English",
        "de" => "German",
        "it" => "Italian",
        "fr" => "French",
        "es" => "Spanish",
        "sl" => "Slovenian",
        "sr" => "Serbian",
        "bs" => "Bosnian",
        "ru" => "Russian",
        "uk" => "Ukrainian",
        "ar" => "Arabic",
        "zh" => "Chinese",
        "ja" => "Japanese",
        _ => "source language",
    }
}

fn segment_times(entry: &Value) -> (f64, f64) {
    if let Some(offsets) = entry.get("offsets") {
        let start = offsets.get("from").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
        let end = offsets
            .get("to")
            .and_then(Value::as_f64)
            .unwrap_or(start * 1000.0)
            / 1000.0;
        return (start, end);
    }
    if let Some(timestamps) = entry.get("timestamps") {
        let start = timestamps
            .get("from")
            .and_then(Value::as_str)
            .and_then(parse_timestamp)
            .unwrap_or(0.0);
        let end = timestamps
            .get("to")
            .and_then(Value::as_str)
            .and_then(parse_timestamp)
            .unwrap_or(start);
        return (start, end);
    }
    (
        entry.get("start").and_then(Value::as_f64).unwrap_or(0.0),
        entry.get("end").and_then(Value::as_f64).unwrap_or(0.0),
    )
}

fn parse_timestamp(raw: &str) -> Option<f64> {
    let normalized = raw.trim().replace(',', ".");
    let mut parts = normalized.split(':');
    let hours = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn sse_event(event: Value) -> String {
    format!("data: {event}\n\n")
}

fn sse_stream_response(receiver: mpsc::Receiver<String>) -> Response {
    let stream =
        ReceiverStream::new(receiver).map(|event| Ok::<Bytes, Infallible>(Bytes::from(event)));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn api_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({
            "detail": {
                "status": "error",
                "message": message.into(),
            }
        })),
    )
}

fn normalize_language(raw: &str) -> String {
    let value = raw.trim().to_ascii_lowercase().replace('_', "-");
    if value.is_empty() || value == "auto" {
        return "auto".into();
    }
    value.split('-').next().unwrap_or("auto").to_string()
}

fn system_language() -> String {
    if let Ok(configured) = std::env::var("QNC_SYSTEM_LANGUAGE") {
        let language = normalize_language(&configured);
        if language != "auto" {
            return language;
        }
    }
    os_system_language().unwrap_or_else(|| "en".into())
}

#[cfg(windows)]
fn os_system_language() -> Option<String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultLocaleName(locale_name: *mut u16, locale_name_count: i32) -> i32;
    }
    let mut buffer = [0_u16; 85];
    let length = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 1 {
        return None;
    }
    let locale = String::from_utf16_lossy(&buffer[..length as usize - 1]);
    Some(normalize_language(&locale))
}

#[cfg(not(windows))]
fn os_system_language() -> Option<String> {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .ok()
        .map(|value| normalize_language(value.split('.').next().unwrap_or(&value)))
        .filter(|value| value != "auto")
}

fn env_flag(name: &str) -> Option<bool> {
    let value = std::env::var(name).ok()?.trim().to_ascii_lowercase();
    Some(matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}

fn resolve_command(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let direct = PathBuf::from(raw);
    if direct.is_file() {
        return Some(direct);
    }
    let names: Vec<String> = if cfg!(windows) && Path::new(raw).extension().is_none() {
        vec![format!("{raw}.exe"), raw.to_string()]
    } else {
        vec![raw.to_string()]
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

fn discover_model(root: &Path) -> Option<PathBuf> {
    const NAMES: &[&str] = &[
        "ggml-small.bin",
        "ggml-large-v3-turbo-q5_0.bin",
        "ggml-large-v3-turbo.bin",
        "ggml-large-v3-q5_0.bin",
        "ggml-large-v3.bin",
        "ggml-medium.bin",
        "ggml-base.bin",
    ];
    for dir in [root.join("models"), root.join("data").join("models")] {
        for name in NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn discover_vad_model(root: &Path) -> Option<PathBuf> {
    const NAMES: &[&str] = &[
        "ggml-silero-v6.2.0.bin",
        "ggml-silero-v6.1.0.bin",
        "ggml-silero-v5.1.2.bin",
    ];
    for dir in [root.join("models"), root.join("data").join("models")] {
        for name in NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn discover_ollama_cli() -> Option<PathBuf> {
    let executable = if cfg!(windows) {
        "ollama.exe"
    } else {
        "ollama"
    };
    let mut candidates = Vec::new();
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local_app_data)
                .join("Programs")
                .join("Ollama")
                .join(executable),
        );
    }
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(program_files).join("Ollama").join(executable));
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn translator_model_available(cli: &Path, model: &str) -> bool {
    let output = Command::new(cli).arg("list").output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let expected = model.trim();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .any(|installed| installed == expected)
}

fn tail(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    let start = chars.len().saturating_sub(max_chars);
    chars[start..].iter().collect::<String>().trim().to_string()
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whisper_cpp_json_segments() {
        let doc = json!({
            "result": { "language": "hr" },
            "transcription": [
                {
                    "timestamps": { "from": "00:00:01,500", "to": "00:00:03,250" },
                    "offsets": { "from": 1500, "to": 3250 },
                    "text": " Dobar dan. "
                },
                {
                    "offsets": { "from": 3250, "to": 5000 },
                    "text": "Ovo je test."
                }
            ]
        });
        let transcript = parse_whisper_json(&doc).expect("parse whisper json");
        assert_eq!(transcript["text"], "Dobar dan. Ovo je test.");
        assert_eq!(transcript["segments"][0]["start"], 1.5);
        assert_eq!(transcript["segments"][0]["end"], 3.25);
        assert_eq!(transcript["language"], "hr");
    }

    #[test]
    fn normalizes_locale_to_whisper_language() {
        assert_eq!(normalize_language("hr-HR"), "hr");
        assert_eq!(normalize_language("EN_us"), "en");
        assert_eq!(normalize_language(""), "auto");
    }

    #[test]
    fn validates_and_shifts_in_out_range() {
        let range = transcription_range(Some(12.3456), Some(18.0))
            .expect("valid range")
            .expect("range present");
        assert_eq!(range, (12.346, 18.0));
        assert!(transcription_range(Some(4.0), Some(3.0)).is_err());
        assert!(transcription_range(Some(1.0), None).is_err());

        let mut transcript = json!({
            "segments": [{ "start": 0.5, "end": 1.75, "text": "Test" }]
        });
        shift_transcript_timestamps(&mut transcript, 12.0);
        assert_eq!(transcript["segments"][0]["start"], 12.5);
        assert_eq!(transcript["segments"][0]["end"], 13.75);
    }

    #[test]
    fn drops_consecutive_duplicate_segments() {
        let doc = json!({
            "transcription": [
                { "offsets": { "from": 0, "to": 1000 }, "text": "Dobar dan." },
                { "offsets": { "from": 1000, "to": 2000 }, "text": "Dobar dan!" },
                { "offsets": { "from": 2000, "to": 3000 }, "text": "Nastavljamo." }
            ]
        });
        let transcript = parse_whisper_json(&doc).expect("parse");
        assert_eq!(transcript["segments"].as_array().map(Vec::len), Some(2));
        assert_eq!(transcript["text"], "Dobar dan. Nastavljamo.");
    }

    #[test]
    fn removes_terminal_control_sequences_from_translation() {
        let raw = "globalnom upra\u{1b}[4D\u{1b}[K\nupravljanju.";
        assert_eq!(clean_translation_output(raw), "globalnom upravljanju.");
        assert_eq!(
            clean_translation_output("\"Prirodni hrvatski prijevod.\""),
            "Prirodni hrvatski prijevod."
        );
    }

    #[test]
    fn parses_realtime_whisper_segment_with_range_offset() {
        let segment = parse_realtime_segment("[00:00:01.250 --> 00:00:03.500] Dobar dan.", 10.0)
            .expect("realtime segment");
        assert_eq!(segment["start"], 11.25);
        assert_eq!(segment["end"], 13.5);
        assert_eq!(segment["text"], "Dobar dan.");
    }
}
