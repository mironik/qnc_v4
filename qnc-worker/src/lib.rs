#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use qnc_service_contracts::{
    JobAck, JobClaimRequest, JobClaimResponse, JobCompleteRequest, JobFailRequest,
    JobHeartbeatRequest, JobHeartbeatResponse, JobLease, ProxyGenerateJobPayload,
    ProxyGenerateJobResult,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_HOST_URL: &str = "http://127.0.0.1:8001";
pub const DEFAULT_POLL_MS: u64 = 500;
pub const DEFAULT_LEASE_MS: u64 = 30_000;
pub const SMOKE_JOB_TYPE: &str = "qnc_worker_smoke";
pub const PROXY_GENERATE_JOB_TYPE: &str = "proxy_generate";

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub worker_id: String,
    pub requested_capabilities: Vec<String>,
    pub poll_interval: Duration,
    pub lease_ms: u64,
}

impl WorkerConfig {
    pub fn new(
        worker_id: impl Into<String>,
        requested_capabilities: Vec<String>,
        poll_ms: u64,
        lease_ms: u64,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            requested_capabilities,
            poll_interval: Duration::from_millis(poll_ms.max(50)),
            lease_ms: lease_ms.max(5_000),
        }
    }

    pub fn claim_capabilities(&self, registry: &HandlerRegistry) -> Vec<String> {
        let supported = registry.capabilities();
        if self.requested_capabilities.is_empty() {
            return supported;
        }
        let requested: HashSet<String> = self
            .requested_capabilities
            .iter()
            .map(|value| normalize_job_type(value))
            .filter(|value| !value.is_empty())
            .collect();
        supported
            .into_iter()
            .filter(|capability| requested.contains(capability))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTick {
    pub playback_active: bool,
    pub claimed: usize,
    pub completed: usize,
    pub failed: usize,
    pub idle_reason: Option<String>,
}

impl WorkerTick {
    fn idle(reason: impl Into<String>) -> Self {
        Self {
            playback_active: false,
            claimed: 0,
            completed: 0,
            failed: 0,
            idle_reason: Some(reason.into()),
        }
    }
}

pub trait WorkerHost: Send + Sync {
    fn claim(&self, request: &JobClaimRequest) -> Result<JobClaimResponse, String>;
    fn heartbeat(&self, request: &JobHeartbeatRequest) -> Result<JobHeartbeatResponse, String>;
    fn complete(&self, request: &JobCompleteRequest) -> Result<JobAck, String>;
    fn fail(&self, request: &JobFailRequest) -> Result<JobAck, String>;
}

pub trait JobHandler: Send + Sync {
    fn job_type(&self) -> &'static str;
    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError>;
}

pub trait ProxyBuilder: Send + Sync {
    fn build_proxy(
        &self,
        payload: ProxyGenerateJobPayload,
    ) -> Result<ProxyGenerateJobResult, JobHandlerError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobHandlerError {
    pub message: String,
    pub retryable: bool,
}

impl JobHandlerError {
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

pub struct HandlerRegistry {
    handlers: HashMap<String, Box<dyn JobHandler>>,
}

impl HandlerRegistry {
    pub fn empty() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn with_builtin_handlers() -> Self {
        let mut registry = Self::empty();
        registry.register(SmokeJobHandler);
        registry
    }

    pub fn register<H>(&mut self, handler: H)
    where
        H: JobHandler + 'static,
    {
        self.handlers
            .insert(normalize_job_type(handler.job_type()), Box::new(handler));
    }

    pub fn capabilities(&self) -> Vec<String> {
        let mut values: Vec<String> = self.handlers.keys().cloned().collect();
        values.sort();
        values
    }

    fn get(&self, job_type: &str) -> Option<&dyn JobHandler> {
        self.handlers
            .get(&normalize_job_type(job_type))
            .map(|handler| handler.as_ref())
    }
}

pub struct Worker<H: WorkerHost> {
    config: WorkerConfig,
    host: H,
    registry: HandlerRegistry,
}

impl<H: WorkerHost> Worker<H> {
    pub fn new(config: WorkerConfig, host: H, registry: HandlerRegistry) -> Self {
        Self {
            config,
            host,
            registry,
        }
    }

    pub fn run_once(&self) -> Result<WorkerTick, String> {
        let capabilities = self.config.claim_capabilities(&self.registry);
        if capabilities.is_empty() {
            return Ok(WorkerTick::idle("no_executable_capabilities"));
        }

        let claim = self.host.claim(&JobClaimRequest {
            worker_id: self.config.worker_id.clone(),
            project_id: None,
            capabilities,
            max_jobs: Some(1),
            lease_ms: Some(self.config.lease_ms),
        })?;

        if claim.playback_active {
            return Ok(WorkerTick {
                playback_active: true,
                claimed: 0,
                completed: 0,
                failed: 0,
                idle_reason: claim.message.or_else(|| Some("playback_active".into())),
            });
        }
        if claim.jobs.is_empty() {
            return Ok(WorkerTick {
                playback_active: false,
                claimed: 0,
                completed: 0,
                failed: 0,
                idle_reason: claim.message.or_else(|| Some("no_jobs".into())),
            });
        }

        let mut completed = 0usize;
        let mut failed = 0usize;
        for job in &claim.jobs {
            match self.run_claimed_job(job) {
                Ok(()) => completed += 1,
                Err(_) => failed += 1,
            }
        }

        Ok(WorkerTick {
            playback_active: false,
            claimed: claim.jobs.len(),
            completed,
            failed,
            idle_reason: None,
        })
    }

    fn run_claimed_job(&self, job: &JobLease) -> Result<(), String> {
        let Some(handler) = self.registry.get(&job.job_type) else {
            return Err(format!(
                "No handler registered for job_type={}",
                job.job_type
            ));
        };

        let heartbeat = self.host.heartbeat(&JobHeartbeatRequest {
            worker_id: self.config.worker_id.clone(),
            project_id: job.project_id.clone(),
            lease_id: job.lease_id.clone(),
            job_ids: vec![job.job_id.clone()],
            lease_ms: Some(self.config.lease_ms),
        })?;
        if !heartbeat.accepted.iter().any(|id| id == &job.job_id) {
            return Err(format!(
                "Lease heartbeat rejected for job_id={}",
                job.job_id
            ));
        }

        match handler.run(job) {
            Ok(result) => {
                let ack = self.host.complete(&JobCompleteRequest {
                    worker_id: self.config.worker_id.clone(),
                    project_id: job.project_id.clone(),
                    lease_id: job.lease_id.clone(),
                    job_id: job.job_id.clone(),
                    result,
                })?;
                if ack.accepted {
                    Ok(())
                } else {
                    Err(ack.message.unwrap_or_else(|| "complete rejected".into()))
                }
            }
            Err(error) => {
                let _ = self.host.fail(&JobFailRequest {
                    worker_id: self.config.worker_id.clone(),
                    project_id: job.project_id.clone(),
                    lease_id: job.lease_id.clone(),
                    job_id: job.job_id.clone(),
                    error: error.message.clone(),
                    retryable: error.retryable,
                });
                Err(error.message)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct HttpJobClient {
    base_url: String,
    agent: ureq::Agent,
}

impl HttpJobClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(5))
                .timeout_read(Duration::from_secs(30))
                .build(),
        }
    }

    fn absolute(&self, rel: &str) -> String {
        if rel.starts_with("http://") || rel.starts_with("https://") {
            rel.to_string()
        } else if rel.starts_with('/') {
            format!("{}{rel}", self.base_url)
        } else {
            format!("{}/{rel}", self.base_url)
        }
    }

    fn post_json<T, R>(&self, rel: &str, payload: &T) -> Result<R, String>
    where
        T: Serialize,
        R: DeserializeOwned,
    {
        let url = self.absolute(rel);
        self.agent
            .post(&url)
            .send_json(serde_json::to_value(payload).map_err(|e| e.to_string())?)
            .map_err(|error| format!("POST {rel}: {error}"))?
            .into_json()
            .map_err(|error| format!("POST {rel} json: {error}"))
    }
}

impl WorkerHost for HttpJobClient {
    fn claim(&self, request: &JobClaimRequest) -> Result<JobClaimResponse, String> {
        self.post_json("/api/jobs/claim", request)
    }

    fn heartbeat(&self, request: &JobHeartbeatRequest) -> Result<JobHeartbeatResponse, String> {
        self.post_json("/api/jobs/heartbeat", request)
    }

    fn complete(&self, request: &JobCompleteRequest) -> Result<JobAck, String> {
        self.post_json("/api/jobs/complete", request)
    }

    fn fail(&self, request: &JobFailRequest) -> Result<JobAck, String> {
        self.post_json("/api/jobs/fail", request)
    }
}

#[derive(Debug, Clone)]
struct SmokeJobHandler;

impl JobHandler for SmokeJobHandler {
    fn job_type(&self) -> &'static str {
        SMOKE_JOB_TYPE
    }

    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError> {
        Ok(json!({
            "status": "ok",
            "job_type": job.job_type,
            "clip_id": job.clip_id,
        }))
    }
}

pub struct ProxyGenerateJobHandler<B: ProxyBuilder> {
    builder: B,
}

impl<B: ProxyBuilder> ProxyGenerateJobHandler<B> {
    pub fn new(builder: B) -> Self {
        Self { builder }
    }
}

impl<B> JobHandler for ProxyGenerateJobHandler<B>
where
    B: ProxyBuilder + 'static,
{
    fn job_type(&self) -> &'static str {
        PROXY_GENERATE_JOB_TYPE
    }

    fn run(&self, job: &JobLease) -> Result<Value, JobHandlerError> {
        let payload: ProxyGenerateJobPayload = serde_json::from_value(job.payload.clone())
            .map_err(|error| JobHandlerError::fatal(format!("invalid proxy payload: {error}")))?;
        let result = self.builder.build_proxy(payload)?;
        serde_json::to_value(result)
            .map_err(|error| JobHandlerError::fatal(format!("invalid proxy result: {error}")))
    }
}

fn normalize_job_type(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeHost {
        claims: Mutex<VecDeque<JobClaimResponse>>,
        claim_requests: Mutex<Vec<JobClaimRequest>>,
        heartbeats: Mutex<Vec<JobHeartbeatRequest>>,
        completes: Mutex<Vec<JobCompleteRequest>>,
        fails: Mutex<Vec<JobFailRequest>>,
    }

    impl FakeHost {
        fn with_claim(response: JobClaimResponse) -> Self {
            let mut claims = VecDeque::new();
            claims.push_back(response);
            Self {
                claims: Mutex::new(claims),
                ..Self::default()
            }
        }
    }

    impl WorkerHost for FakeHost {
        fn claim(&self, request: &JobClaimRequest) -> Result<JobClaimResponse, String> {
            self.claim_requests.lock().unwrap().push(request.clone());
            self.claims
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "no fake claim response".to_string())
        }

        fn heartbeat(&self, request: &JobHeartbeatRequest) -> Result<JobHeartbeatResponse, String> {
            self.heartbeats.lock().unwrap().push(request.clone());
            Ok(JobHeartbeatResponse {
                accepted: request.job_ids.clone(),
                rejected: Vec::new(),
                lease_until_unix_ms: 123,
            })
        }

        fn complete(&self, request: &JobCompleteRequest) -> Result<JobAck, String> {
            self.completes.lock().unwrap().push(request.clone());
            Ok(JobAck {
                accepted: true,
                job_id: request.job_id.clone(),
                message: None,
            })
        }

        fn fail(&self, request: &JobFailRequest) -> Result<JobAck, String> {
            self.fails.lock().unwrap().push(request.clone());
            Ok(JobAck {
                accepted: true,
                job_id: request.job_id.clone(),
                message: None,
            })
        }
    }

    fn smoke_lease() -> JobLease {
        JobLease {
            job_id: "qnc_worker_smoke:worker:clip_a".into(),
            project_id: "project_a".into(),
            job_type: SMOKE_JOB_TYPE.into(),
            source_id: "worker".into(),
            clip_id: "clip_a".into(),
            worker_id: "worker_a".into(),
            lease_id: "lease_a".into(),
            lease_until_unix_ms: 123,
            attempts: 1,
            queued_at: Some("epoch_1".into()),
            payload: json!({}),
        }
    }

    struct FakeProxyBuilder;

    impl ProxyBuilder for FakeProxyBuilder {
        fn build_proxy(
            &self,
            payload: ProxyGenerateJobPayload,
        ) -> Result<ProxyGenerateJobResult, JobHandlerError> {
            Ok(ProxyGenerateJobResult {
                output_path: payload.output_path,
                probe: None,
            })
        }
    }

    #[test]
    fn requested_capabilities_are_limited_to_registered_handlers() {
        let config = WorkerConfig::new(
            "worker_a",
            vec!["proxy_generate".into(), SMOKE_JOB_TYPE.into()],
            DEFAULT_POLL_MS,
            DEFAULT_LEASE_MS,
        );
        let caps = config.claim_capabilities(&HandlerRegistry::with_builtin_handlers());
        assert_eq!(caps, vec![SMOKE_JOB_TYPE.to_string()]);
    }

    #[test]
    fn run_once_does_not_call_host_without_executable_capability() {
        let config = WorkerConfig::new(
            "worker_a",
            vec!["proxy_generate".into()],
            DEFAULT_POLL_MS,
            DEFAULT_LEASE_MS,
        );
        let host = FakeHost::default();
        let worker = Worker::new(config, host, HandlerRegistry::with_builtin_handlers());

        let tick = worker.run_once().unwrap();
        assert_eq!(tick, WorkerTick::idle("no_executable_capabilities"));
        assert!(worker.host.claim_requests.lock().unwrap().is_empty());
    }

    #[test]
    fn run_once_heartbeats_and_completes_smoke_job() {
        let host = FakeHost::with_claim(JobClaimResponse {
            jobs: vec![smoke_lease()],
            playback_active: false,
            message: None,
        });
        let config = WorkerConfig::new("worker_a", vec![], DEFAULT_POLL_MS, DEFAULT_LEASE_MS);
        let worker = Worker::new(config, host, HandlerRegistry::with_builtin_handlers());

        let tick = worker.run_once().unwrap();
        assert_eq!(
            tick,
            WorkerTick {
                playback_active: false,
                claimed: 1,
                completed: 1,
                failed: 0,
                idle_reason: None,
            }
        );
        assert_eq!(worker.host.heartbeats.lock().unwrap().len(), 1);
        let completes = worker.host.completes.lock().unwrap();
        assert_eq!(completes.len(), 1);
        assert_eq!(completes[0].job_id, "qnc_worker_smoke:worker:clip_a");
        assert_eq!(completes[0].result["status"], "ok");
    }

    #[test]
    fn proxy_generate_handler_roundtrips_payload_and_result() {
        let handler = ProxyGenerateJobHandler::new(FakeProxyBuilder);
        let output_path = PathBuf::from("C:/qnc/project/proxy/clip_a.mxf");
        let payload = ProxyGenerateJobPayload {
            source_path: PathBuf::from("C:/card/ClipA.MXF"),
            output_path: output_path.clone(),
            asset_status: "ready".into(),
            card_locked: false,
            original_path: Some(PathBuf::from("C:/card/ClipA.MXF")),
        };
        let job = JobLease {
            job_id: "proxy_generate:card:clip_a".into(),
            project_id: "project_a".into(),
            job_type: PROXY_GENERATE_JOB_TYPE.into(),
            source_id: "card".into(),
            clip_id: "clip_a".into(),
            worker_id: "worker_a".into(),
            lease_id: "lease_a".into(),
            lease_until_unix_ms: 123,
            attempts: 1,
            queued_at: None,
            payload: serde_json::to_value(payload).unwrap(),
        };

        let result = handler.run(&job).unwrap();
        let decoded: ProxyGenerateJobResult = serde_json::from_value(result).unwrap();
        assert_eq!(decoded.output_path, output_path);
        assert!(decoded.probe.is_none());
    }
}
