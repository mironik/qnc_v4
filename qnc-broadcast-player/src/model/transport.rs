use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportStatus {
    Empty,
    Ready,
    Playing,
    Paused,
    Stopped,
}
