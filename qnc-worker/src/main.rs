use std::process::ExitCode;
use std::thread;

use clap::{Parser, ValueEnum};
use qnc_service_contracts::WorkerPlacement;
use qnc_worker::{
    detect_worker_placement, HandlerRegistry, HttpJobClient, Worker, WorkerConfig,
    DEFAULT_HOST_URL, DEFAULT_LEASE_MS, DEFAULT_POLL_MS,
};

#[derive(Debug, Parser)]
#[command(name = "qnc-worker")]
#[command(about = "External QNC worker for host JobService jobs")]
struct Args {
    #[arg(long, env = "QNC_HOST_URL", default_value = DEFAULT_HOST_URL)]
    host_url: String,

    #[arg(long, env = "QNC_WORKER_ID")]
    worker_id: Option<String>,

    #[arg(
        long = "capability",
        env = "QNC_WORKER_CAPABILITIES",
        value_delimiter = ','
    )]
    capabilities: Vec<String>,

    #[arg(long, env = "QNC_WORKER_POLL_MS", default_value_t = DEFAULT_POLL_MS)]
    poll_ms: u64,

    #[arg(long, env = "QNC_WORKER_LEASE_MS", default_value_t = DEFAULT_LEASE_MS)]
    lease_ms: u64,

    #[arg(long, env = "QNC_WORKER_PLACEMENT", value_enum)]
    placement: Option<PlacementArg>,

    #[arg(long)]
    once: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PlacementArg {
    #[value(name = "local_workstation", alias = "local", alias = "workstation")]
    LocalWorkstation,
    #[value(
        name = "intranet_shared_media",
        alias = "intranet",
        alias = "shared_media"
    )]
    IntranetSharedMedia,
}

impl From<PlacementArg> for WorkerPlacement {
    fn from(value: PlacementArg) -> Self {
        match value {
            PlacementArg::LocalWorkstation => WorkerPlacement::LocalWorkstation,
            PlacementArg::IntranetSharedMedia => WorkerPlacement::IntranetSharedMedia,
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let worker_id = args.worker_id.unwrap_or_else(default_worker_id);
    let auto_placement = detect_worker_placement(&args.host_url);
    let placement = args
        .placement
        .map(WorkerPlacement::from)
        .unwrap_or(auto_placement);
    let placement_source = if args.placement.is_some() {
        "manual"
    } else {
        "auto"
    };
    let registry = HandlerRegistry::with_builtin_handlers();
    let config = WorkerConfig::new(
        worker_id.clone(),
        args.capabilities,
        args.poll_ms,
        args.lease_ms,
    )
    .with_placement(placement);
    let executable_capabilities = config.claim_capabilities(&registry);
    let host = HttpJobClient::new(&args.host_url);
    let worker = Worker::new(config.clone(), host, registry);

    eprintln!(
        "qnc-worker: host={} worker_id={} placement={:?} placement_source={} capabilities={:?}",
        args.host_url, worker_id, placement, placement_source, executable_capabilities
    );

    loop {
        match worker.run_once() {
            Ok(tick) => {
                if tick.claimed > 0 || tick.playback_active {
                    eprintln!("qnc-worker: tick={tick:?}");
                }
            }
            Err(error) => {
                eprintln!("qnc-worker: error={error}");
                if args.once {
                    return ExitCode::from(1);
                }
            }
        }
        if args.once {
            return ExitCode::SUCCESS;
        }
        thread::sleep(config.poll_interval);
    }
}

fn default_worker_id() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "local".into());
    format!("qnc-worker-{}-{}", sanitize(&host), std::process::id())
}

fn sanitize(value: &str) -> String {
    let mut out: String = value
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out = "local".into();
    }
    out
}
