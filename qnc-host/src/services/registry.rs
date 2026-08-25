use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use qnc_service_contracts::{
    AIOrchestrator, AiRequest, AiResponse, ArtifactRef, AudioProbe, AudioProbeRequest,
    AudioWrapRequest, ExtractRangeRequest, FilmstripFrameArtifact, FilmstripRequest,
    FrameExtractRequest, MediaProbe, MediaProcessor, MediaRef, PosterExtractRequest,
    ProxyBuildRequest, SearchDocument, SearchEngine, SearchHit, SearchRequest, ServiceRegistry,
    Transcript, TranscriptionEngine, TranscriptionRequest, WaveformPeaks, WaveformRequest,
};
use qnc_service_contracts::{
    ExportEngine, ExportJob, ExportJobState, ExportRequest, ServiceError, ServiceResult,
};
use serde_json::{json, Value};

use crate::config::{RuntimeConfig, ServiceBackendConfig};

use super::export_process::ExternalProcessExportEngine;
#[cfg(test)]
use super::media_ffmpeg::LocalFfmpegMediaProcessor;

#[cfg(test)]
pub fn build_registry(config: &RuntimeConfig) -> ServiceRegistry {
    ServiceRegistry {
        media: build_media_processor(&config.media),
        transcription: build_transcription_engine(&config.transcription),
        search: build_search_engine(&config.search),
        ai: build_ai_orchestrator(&config.ai),
        export: build_export_engine(&config.export),
    }
}

pub fn describe_runtime(config: &RuntimeConfig) -> Value {
    json!({
        "profile": config.profile,
        "deployment": config.deployment,
        "workers": {
            "artifact_owner": "jobservice",
        },
        "integration": {
            "gateway": {
                "kind": config.integration.gateway.kind,
                "endpoint": config.integration.gateway.endpoint,
                "read_only": config.integration.gateway.read_only,
                "contract": "read_through_non_invasive_media_gateway",
            },
        },
        "services": {
            "media": describe_media_backend(&config.media),
            "transcription": describe_optional_backend(
                "transcription",
                &config.transcription,
                &[],
            ),
            "search": describe_optional_backend("search", &config.search, &[]),
            "ai": describe_optional_backend("ai", &config.ai, &[]),
            "export": describe_export_backend(&config.export),
        },
    })
}

#[cfg(test)]
fn build_media_processor(config: &ServiceBackendConfig) -> Arc<dyn MediaProcessor> {
    let backend = normalized_backend(config, "local_ffmpeg");
    match backend.as_str() {
        "local_ffmpeg" | "ffmpeg" => Arc::new(LocalFfmpegMediaProcessor::new()),
        _ => Arc::new(UnavailableMediaProcessor::new(backend)),
    }
}

#[cfg(test)]
fn build_transcription_engine(config: &ServiceBackendConfig) -> Arc<dyn TranscriptionEngine> {
    let backend = normalized_backend(config, "disabled");
    match backend.as_str() {
        "disabled" | "none" | "off" => Arc::new(DisabledTranscriptionEngine),
        _ => Arc::new(UnavailableTranscriptionEngine::new(backend)),
    }
}

#[cfg(test)]
fn build_search_engine(config: &ServiceBackendConfig) -> Arc<dyn SearchEngine> {
    let backend = normalized_backend(config, "disabled");
    match backend.as_str() {
        "disabled" | "none" | "off" => Arc::new(DisabledSearchEngine),
        _ => Arc::new(UnavailableSearchEngine::new(backend)),
    }
}

#[cfg(test)]
fn build_ai_orchestrator(config: &ServiceBackendConfig) -> Arc<dyn AIOrchestrator> {
    let backend = normalized_backend(config, "disabled");
    match backend.as_str() {
        "disabled" | "none" | "off" => Arc::new(DisabledAiOrchestrator),
        _ => Arc::new(UnavailableAiOrchestrator::new(backend)),
    }
}

pub fn build_export_engine(config: &ServiceBackendConfig) -> Arc<dyn ExportEngine> {
    let backend = normalized_backend(config, "disabled");
    match backend.as_str() {
        "disabled" | "none" | "off" => Arc::new(DisabledExportEngine),
        "external_process" => configured_command(config)
            .map(ExternalProcessExportEngine::new)
            .map(|engine| Arc::new(engine) as Arc<dyn ExportEngine>)
            .unwrap_or_else(|| Arc::new(MisconfiguredExportEngine::external_process())),
        _ => Arc::new(UnavailableExportEngine::new(backend)),
    }
}

fn normalized_backend(config: &ServiceBackendConfig, default_backend: &str) -> String {
    let backend = config.backend.trim();
    if backend.is_empty() {
        default_backend.to_string()
    } else {
        backend.to_ascii_lowercase()
    }
}

fn describe_media_backend(config: &ServiceBackendConfig) -> Value {
    let backend = normalized_backend(config, "local_ffmpeg");
    match backend.as_str() {
        "local_ffmpeg" | "ffmpeg" => service_description(
            &backend,
            "active",
            true,
            "Local FFmpeg media processor is active.",
            config,
        ),
        _ => service_description(
            &backend,
            "unavailable",
            false,
            "Configured media backend is not implemented yet.",
            config,
        ),
    }
}

fn describe_export_backend(config: &ServiceBackendConfig) -> Value {
    let backend = normalized_backend(config, "disabled");
    match backend.as_str() {
        "disabled" | "none" | "off" => service_description(
            &backend,
            "disabled",
            true,
            "export service is disabled.",
            config,
        ),
        "external_process" if configured_command(config).is_some() => service_description(
            &backend,
            "active",
            true,
            "External export process adapter is active.",
            config,
        ),
        "external_process" => service_description(
            &backend,
            "unavailable",
            false,
            "External export process requires [export].command.",
            config,
        ),
        "remote_rest" => service_description(
            &backend,
            "unavailable",
            false,
            "Remote REST export adapter is not implemented yet.",
            config,
        ),
        _ => service_description(
            &backend,
            "unavailable",
            false,
            "Configured export backend is not implemented yet.",
            config,
        ),
    }
}

fn describe_optional_backend(
    service: &'static str,
    config: &ServiceBackendConfig,
    implemented_backends: &[&str],
) -> Value {
    let backend = normalized_backend(config, "disabled");
    match backend.as_str() {
        "disabled" | "none" | "off" => service_description(
            &backend,
            "disabled",
            true,
            &format!("{service} service is disabled."),
            config,
        ),
        _ if implemented_backends.contains(&backend.as_str()) => service_description(
            &backend,
            "active",
            true,
            &format!("{service} service is active."),
            config,
        ),
        _ => service_description(
            &backend,
            "unavailable",
            false,
            &format!("Configured {service} backend is not implemented yet."),
            config,
        ),
    }
}

fn service_description(
    backend: &str,
    status: &str,
    implemented: bool,
    message: &str,
    config: &ServiceBackendConfig,
) -> Value {
    json!({
        "backend": backend,
        "status": status,
        "implemented": implemented,
        "message": message,
        "endpoint_configured": config.endpoint.as_ref().is_some_and(|v| !v.trim().is_empty()),
        "command_configured": config.command.as_ref().is_some_and(|v| !v.trim().is_empty()),
        "model_configured": config.model.as_ref().is_some_and(|v| !v.trim().is_empty()),
        "model_path_configured": config.model_path.as_ref().is_some_and(|v| !v.trim().is_empty()),
    })
}

fn configured_command(config: &ServiceBackendConfig) -> Option<String> {
    config
        .command
        .as_deref()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct UnavailableMediaProcessor {
    backend: String,
}

#[cfg(test)]
impl UnavailableMediaProcessor {
    fn new(backend: String) -> Self {
        Self { backend }
    }

    fn error(&self) -> ServiceError {
        unavailable("media_backend_unavailable", "media", &self.backend)
    }
}

#[cfg(test)]
#[async_trait]
impl MediaProcessor for UnavailableMediaProcessor {
    async fn probe(&self, _input: &MediaRef) -> ServiceResult<MediaProbe> {
        Err(self.error())
    }

    async fn probe_audio(&self, _request: AudioProbeRequest) -> ServiceResult<AudioProbe> {
        Err(self.error())
    }

    async fn extract_frame(&self, _request: FrameExtractRequest) -> ServiceResult<ArtifactRef> {
        Err(self.error())
    }

    async fn extract_poster(&self, _request: PosterExtractRequest) -> ServiceResult<ArtifactRef> {
        Err(self.error())
    }

    async fn build_filmstrip(
        &self,
        _request: FilmstripRequest,
    ) -> ServiceResult<Vec<FilmstripFrameArtifact>> {
        Err(self.error())
    }

    async fn build_proxy(&self, _request: ProxyBuildRequest) -> ServiceResult<ArtifactRef> {
        Err(self.error())
    }

    async fn build_audio_wrap(&self, _request: AudioWrapRequest) -> ServiceResult<ArtifactRef> {
        Err(self.error())
    }

    async fn build_waveform(&self, _request: WaveformRequest) -> ServiceResult<WaveformPeaks> {
        Err(self.error())
    }

    async fn extract_range(&self, _request: ExtractRangeRequest) -> ServiceResult<ArtifactRef> {
        Err(self.error())
    }
}

#[cfg(test)]
#[derive(Debug, Default, Clone)]
struct DisabledTranscriptionEngine;

#[cfg(test)]
#[async_trait]
impl TranscriptionEngine for DisabledTranscriptionEngine {
    async fn transcribe(&self, _request: TranscriptionRequest) -> ServiceResult<Transcript> {
        Err(disabled(
            "transcription_disabled",
            "Transcription service is disabled.",
        ))
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct UnavailableTranscriptionEngine {
    backend: String,
}

#[cfg(test)]
impl UnavailableTranscriptionEngine {
    fn new(backend: String) -> Self {
        Self { backend }
    }
}

#[cfg(test)]
#[async_trait]
impl TranscriptionEngine for UnavailableTranscriptionEngine {
    async fn transcribe(&self, _request: TranscriptionRequest) -> ServiceResult<Transcript> {
        Err(unavailable(
            "transcription_backend_unavailable",
            "transcription",
            &self.backend,
        ))
    }
}

#[cfg(test)]
#[derive(Debug, Default, Clone)]
struct DisabledSearchEngine;

#[cfg(test)]
#[async_trait]
impl SearchEngine for DisabledSearchEngine {
    async fn index_document(&self, _document: SearchDocument) -> ServiceResult<()> {
        Ok(())
    }

    async fn remove_clip(&self, _clip_id: &str) -> ServiceResult<()> {
        Ok(())
    }

    async fn search(&self, _request: SearchRequest) -> ServiceResult<Vec<SearchHit>> {
        Err(disabled("search_disabled", "Search service is disabled."))
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct UnavailableSearchEngine {
    backend: String,
}

#[cfg(test)]
impl UnavailableSearchEngine {
    fn new(backend: String) -> Self {
        Self { backend }
    }
}

#[cfg(test)]
#[async_trait]
impl SearchEngine for UnavailableSearchEngine {
    async fn index_document(&self, _document: SearchDocument) -> ServiceResult<()> {
        Err(unavailable(
            "search_backend_unavailable",
            "search",
            &self.backend,
        ))
    }

    async fn remove_clip(&self, _clip_id: &str) -> ServiceResult<()> {
        Err(unavailable(
            "search_backend_unavailable",
            "search",
            &self.backend,
        ))
    }

    async fn search(&self, _request: SearchRequest) -> ServiceResult<Vec<SearchHit>> {
        Err(unavailable(
            "search_backend_unavailable",
            "search",
            &self.backend,
        ))
    }
}

#[cfg(test)]
#[derive(Debug, Default, Clone)]
struct DisabledAiOrchestrator;

#[cfg(test)]
#[async_trait]
impl AIOrchestrator for DisabledAiOrchestrator {
    async fn run(&self, _request: AiRequest) -> ServiceResult<AiResponse> {
        Err(disabled("ai_disabled", "AI orchestrator is disabled."))
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct UnavailableAiOrchestrator {
    backend: String,
}

#[cfg(test)]
impl UnavailableAiOrchestrator {
    fn new(backend: String) -> Self {
        Self { backend }
    }
}

#[cfg(test)]
#[async_trait]
impl AIOrchestrator for UnavailableAiOrchestrator {
    async fn run(&self, _request: AiRequest) -> ServiceResult<AiResponse> {
        Err(unavailable("ai_backend_unavailable", "ai", &self.backend))
    }
}

#[derive(Debug, Default, Clone)]
struct DisabledExportEngine;

#[async_trait]
impl ExportEngine for DisabledExportEngine {
    async fn submit(&self, _request: ExportRequest) -> ServiceResult<ExportJob> {
        Err(disabled("export_disabled", "Export service is disabled."))
    }

    async fn status(&self, job_id: &str) -> ServiceResult<ExportJob> {
        Ok(ExportJob {
            job_id: job_id.to_string(),
            state: ExportJobState::Cancelled,
            artifacts: vec![],
            message: Some("Export service is disabled.".into()),
        })
    }

    async fn cancel(&self, _job_id: &str) -> ServiceResult<()> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct MisconfiguredExportEngine {
    code: &'static str,
    message: &'static str,
}

impl MisconfiguredExportEngine {
    fn external_process() -> Self {
        Self {
            code: "export_command_required",
            message: "Export backend external_process requires [export].command.",
        }
    }
}

#[async_trait]
impl ExportEngine for MisconfiguredExportEngine {
    async fn submit(&self, _request: ExportRequest) -> ServiceResult<ExportJob> {
        Err(disabled(self.code, self.message))
    }

    async fn status(&self, _job_id: &str) -> ServiceResult<ExportJob> {
        Err(disabled(self.code, self.message))
    }

    async fn cancel(&self, _job_id: &str) -> ServiceResult<()> {
        Err(disabled(self.code, self.message))
    }
}

#[derive(Debug, Clone)]
struct UnavailableExportEngine {
    backend: String,
}

impl UnavailableExportEngine {
    fn new(backend: String) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ExportEngine for UnavailableExportEngine {
    async fn submit(&self, _request: ExportRequest) -> ServiceResult<ExportJob> {
        Err(unavailable(
            "export_backend_unavailable",
            "export",
            &self.backend,
        ))
    }

    async fn status(&self, _job_id: &str) -> ServiceResult<ExportJob> {
        Err(unavailable(
            "export_backend_unavailable",
            "export",
            &self.backend,
        ))
    }

    async fn cancel(&self, _job_id: &str) -> ServiceResult<()> {
        Err(unavailable(
            "export_backend_unavailable",
            "export",
            &self.backend,
        ))
    }
}

fn disabled(code: &'static str, message: &'static str) -> ServiceError {
    ServiceError::new(code, message)
}

fn unavailable(code: &'static str, service: &'static str, backend: &str) -> ServiceError {
    ServiceError::new(
        code,
        format!("{service} backend '{backend}' is configured but not implemented yet."),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use qnc_service_contracts::{
        AiRequest, MediaLocator, MediaRef, RuntimeProfile, SearchRequest, TranscriptionRequest,
    };

    use super::*;

    fn backend(name: &str) -> ServiceBackendConfig {
        ServiceBackendConfig {
            backend: name.to_string(),
            endpoint: None,
            command: None,
            model: None,
            model_path: None,
        }
    }

    fn runtime_config(
        media: &str,
        transcription: &str,
        search: &str,
        ai: &str,
        export: &str,
    ) -> RuntimeConfig {
        RuntimeConfig {
            profile: RuntimeProfile::Light,
            deployment: Default::default(),
            workers: Default::default(),
            integration: Default::default(),
            media: backend(media),
            transcription: backend(transcription),
            search: backend(search),
            ai: backend(ai),
            export: backend(export),
        }
    }

    fn missing_media_ref() -> MediaRef {
        MediaRef {
            clip_id: "missing".into(),
            locator: MediaLocator::LocalPath {
                path: PathBuf::from("missing.mov"),
            },
        }
    }

    #[tokio::test]
    async fn local_ffmpeg_media_backend_is_selected() {
        let registry = build_registry(&runtime_config(
            "local_ffmpeg",
            "disabled",
            "disabled",
            "disabled",
            "disabled",
        ));
        let err = registry
            .media
            .probe(&missing_media_ref())
            .await
            .unwrap_err();

        assert_eq!(err.code, "probe_failed");
    }

    #[tokio::test]
    async fn remote_media_backend_is_explicitly_unavailable() {
        let registry = build_registry(&runtime_config(
            "remote_rest",
            "disabled",
            "disabled",
            "disabled",
            "disabled",
        ));
        let err = registry
            .media
            .probe(&missing_media_ref())
            .await
            .unwrap_err();

        assert_eq!(err.code, "media_backend_unavailable");
        assert!(err.message.contains("remote_rest"));
    }

    #[tokio::test]
    async fn configured_ai_backend_is_explicitly_unavailable() {
        let registry = build_registry(&runtime_config(
            "local_ffmpeg",
            "disabled",
            "disabled",
            "ollama",
            "disabled",
        ));
        let err = registry
            .ai
            .run(AiRequest {
                task: "summarize".into(),
                input: serde_json::json!({}),
            })
            .await
            .unwrap_err();

        assert_eq!(err.code, "ai_backend_unavailable");
        assert!(err.message.contains("ollama"));
    }

    #[tokio::test]
    async fn default_disabled_backends_return_disabled_errors() {
        let registry = build_registry(&runtime_config(
            "local_ffmpeg",
            "disabled",
            "disabled",
            "disabled",
            "disabled",
        ));

        let transcription_err = registry
            .transcription
            .transcribe(TranscriptionRequest {
                input: missing_media_ref(),
                range: None,
                language: None,
            })
            .await
            .unwrap_err();
        let search_err = registry
            .search
            .search(SearchRequest {
                query: "test".into(),
                limit: 10,
            })
            .await
            .unwrap_err();
        let ai_err = registry
            .ai
            .run(AiRequest {
                task: "test".into(),
                input: serde_json::json!({}),
            })
            .await
            .unwrap_err();
        let export_err = registry
            .export
            .submit(ExportRequest {
                project_id: "p1".into(),
                playlist: serde_json::json!({}),
                project_settings: serde_json::json!({}),
                export_settings: serde_json::json!({}),
                output_dir: None,
            })
            .await
            .unwrap_err();

        assert_eq!(transcription_err.code, "transcription_disabled");
        assert_eq!(search_err.code, "search_disabled");
        assert_eq!(ai_err.code, "ai_disabled");
        assert_eq!(export_err.code, "export_disabled");
    }

    #[test]
    fn runtime_description_reports_effective_adapter_status() {
        let description = describe_runtime(&runtime_config(
            "remote_rest",
            "whisper_cpp",
            "disabled",
            "ollama",
            "external_process",
        ));

        assert_eq!(description["profile"], "light");
        assert_eq!(description["deployment"], "single_workstation");
        assert_eq!(description["services"]["media"]["backend"], "remote_rest");
        assert_eq!(description["services"]["media"]["status"], "unavailable");
        assert_eq!(description["integration"]["gateway"]["kind"], "local_fs");
        assert_eq!(
            description["integration"]["gateway"]["contract"],
            "read_through_non_invasive_media_gateway"
        );
        assert_eq!(description["integration"]["gateway"]["read_only"], true);
        assert_eq!(
            description["services"]["transcription"]["status"],
            "unavailable"
        );
        assert_eq!(description["services"]["search"]["status"], "disabled");
        assert_eq!(description["services"]["ai"]["backend"], "ollama");
        assert_eq!(
            description["services"]["export"]["backend"],
            "external_process"
        );
        assert_eq!(description["services"]["export"]["status"], "unavailable");
    }

    #[test]
    fn runtime_description_reports_external_process_active_with_command() {
        let mut config = runtime_config(
            "local_ffmpeg",
            "disabled",
            "disabled",
            "disabled",
            "external_process",
        );
        config.export.command = Some("qnc-export-plugin".into());

        let description = describe_runtime(&config);

        assert_eq!(
            description["services"]["export"]["backend"],
            "external_process"
        );
        assert_eq!(description["services"]["export"]["status"], "active");
        assert_eq!(description["services"]["export"]["implemented"], true);
        assert_eq!(
            description["services"]["export"]["command_configured"],
            true
        );
    }

    #[tokio::test]
    async fn external_process_without_command_returns_configuration_error() {
        let registry = build_registry(&runtime_config(
            "local_ffmpeg",
            "disabled",
            "disabled",
            "disabled",
            "external_process",
        ));

        let err = registry
            .export
            .submit(ExportRequest {
                project_id: "p1".into(),
                playlist: serde_json::json!({}),
                project_settings: serde_json::json!({}),
                export_settings: serde_json::json!({}),
                output_dir: None,
            })
            .await
            .unwrap_err();

        assert_eq!(err.code, "export_command_required");
    }
}
