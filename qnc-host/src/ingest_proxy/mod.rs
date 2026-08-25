mod worker;

pub use worker::ProxyGenerateWorker;

/// True while proxy ffmpeg jobs are queued or running.
#[allow(dead_code)] // kept for diagnostics; workers defer per missing media_path instead
pub fn proxy_generate_busy(_paths: &crate::project::db::ProjectPaths) -> bool {
    false
}
