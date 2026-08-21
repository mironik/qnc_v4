# qnc-client (legacy F4 transport)

This crate is parked legacy code. It is intentionally excluded from the root
workspace and is not the product UI owner for `qnc_v4`.

Active product UI is `qnc-app`; active playback goes through the broadcast
player stack. Do not use this crate as an architecture reference for timeline,
fps, story, or playback behavior.

## Build

```powershell
cargo build --manifest-path qnc-client/Cargo.toml --release
```

## Usage

```powershell
cargo run --manifest-path qnc-client/Cargo.toml -- health

# One-shot: frame + rodio mixed audio chunk
cargo run --manifest-path qnc-client/Cargo.toml -- play --project-id <id> --seek 1.0 --audio

# Transport GUI: play/pause/±1s + frame loop + rodio
cargo run --manifest-path qnc-client/Cargo.toml -- play --project-id <id> --seek 0 --gui
```
