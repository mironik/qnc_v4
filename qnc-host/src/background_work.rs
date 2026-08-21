use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{SystemTime, UNIX_EPOCH};

const PLAYBACK_ACTIVE_LEASE_MS: u64 = 5_000;

#[derive(Clone, Default)]
pub struct BackgroundWorkGate {
    playback_active: Arc<AtomicBool>,
    playback_seen_ms: Arc<AtomicU64>,
}

impl BackgroundWorkGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_playback_active(&self, active: bool) {
        self.playback_active.store(active, Ordering::Release);
        self.playback_seen_ms.store(now_ms(), Ordering::Release);
    }

    pub fn playback_active(&self) -> bool {
        if !self.playback_active.load(Ordering::Acquire) {
            return false;
        }
        let seen = self.playback_seen_ms.load(Ordering::Acquire);
        seen > 0 && now_ms().saturating_sub(seen) <= PLAYBACK_ACTIVE_LEASE_MS
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}
