mod worker;

pub use worker::ProxyGenerateWorker;

use std::sync::atomic::{AtomicUsize, Ordering};

/// Live proxy encode pressure (not stale DB `generating_proxy` rows).
static PROXY_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static PROXY_QUEUED: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn proxy_job_queued(delta: isize) {
    if delta >= 0 {
        PROXY_QUEUED.fetch_add(delta as usize, Ordering::AcqRel);
    } else {
        PROXY_QUEUED.fetch_sub((-delta) as usize, Ordering::AcqRel);
    }
}

pub(crate) fn proxy_job_begin() {
    PROXY_IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
}

pub(crate) fn proxy_job_end() {
    PROXY_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
}

/// True while proxy ffmpeg jobs are queued or running.
#[allow(dead_code)] // kept for diagnostics; workers defer per missing media_path instead
pub fn proxy_generate_busy(_paths: &crate::project::db::ProjectPaths) -> bool {
    PROXY_IN_FLIGHT.load(Ordering::Acquire) > 0 || PROXY_QUEUED.load(Ordering::Acquire) > 0
}
