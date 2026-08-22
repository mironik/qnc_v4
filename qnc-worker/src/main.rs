use std::process::ExitCode;
use std::thread;

use clap::Parser;
use qnc_worker::{
    HandlerRegistry, HttpJobClient, Worker, WorkerConfig, DEFAULT_HOST_URL, DEFAULT_LEASE_MS,
    DEFAULT_POLL_MS,
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

    #[arg(long)]
    once: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let worker_id = args.worker_id.unwrap_or_else(default_worker_id);
    let registry = HandlerRegistry::with_builtin_handlers();
    let config = WorkerConfig::new(
        worker_id.clone(),
        args.capabilities,
        args.poll_ms,
        args.lease_ms,
    );
    let executable_capabilities = config.claim_capabilities(&registry);
    let host = HttpJobClient::new(&args.host_url);
    let worker = Worker::new(config.clone(), host, registry);

    eprintln!(
        "qnc-worker: host={} worker_id={} capabilities={:?}",
        args.host_url, worker_id, executable_capabilities
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
