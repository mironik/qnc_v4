# qnc-client (F4 transport)

Native client against `qnc-host` HTTP playback API (proxy-first).

## Build

```powershell
cargo build -p qnc-client --release
```

## Usage

```powershell
cargo run -p qnc-client -- health

# One-shot: frame + rodio mixed audio chunk
cargo run -p qnc-client -- play --project-id <id> --seek 1.0 --audio

# Transport GUI: play/pause/±1s + frame loop + rodio
cargo run -p qnc-client -- play --project-id <id> --seek 0 --gui
```

## Next (F5.3+)

- O/T layer intents → host → reload timeline
- Wrap stack (multiple segment rows / vertical stack)
