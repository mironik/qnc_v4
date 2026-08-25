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
        self.playback_active_at(now_ms())
    }

    fn playback_active_at(&self, now_ms: u64) -> bool {
        let seen = self.playback_seen_ms.load(Ordering::Acquire);
        if seen == 0 {
            return false;
        }
        let elapsed = now_ms.saturating_sub(seen);
        if self.playback_active.load(Ordering::Acquire) {
            elapsed <= PLAYBACK_ACTIVE_LEASE_MS
        } else {
            false
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_gate_releases_immediately_after_inactive_signal() {
        let gate = BackgroundWorkGate::new();
        gate.playback_seen_ms.store(1_000, Ordering::Release);
        gate.playback_active.store(false, Ordering::Release);

        assert!(!gate.playback_active_at(1_000));
    }

    #[test]
    fn playback_gate_active_signal_uses_short_lease() {
        let gate = BackgroundWorkGate::new();
        gate.playback_seen_ms.store(1_000, Ordering::Release);
        gate.playback_active.store(true, Ordering::Release);

        assert!(gate.playback_active_at(1_000 + PLAYBACK_ACTIVE_LEASE_MS));
        assert!(!gate.playback_active_at(1_001 + PLAYBACK_ACTIVE_LEASE_MS));
    }
}
