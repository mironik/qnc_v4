# QNC — native (v0.5.0)

Aktivna radna kopija: **`qnc-app` + `qnc-host`**. Web UI nije dio `qnc_v4`
produkta i legacy `web-arhive/` folder nije prisutan u ovom treeu.

## Pokretanje

```powershell
cd C:\Users\miron\Projects\qnc_v4
.\run_host.ps1
.\run_app.ps1
```

Host je **samo API** (`/api/...`). Nema `/app` HTML/JS.

Vanjski worker za teske artifact poslove pokrece se zasebno:

```powershell
.\run_worker.ps1
```

Default capability je `proxy_generate`; `QNC_WORKER_CAPABILITIES` moze zadati
drugi popis kad dodamo nove vanjske workere.

LAN: `QNC_BIND_HOST=0.0.0.0` zahtijeva `QNC_TRUSTED_LAN=1`. Internet bez auth/proxy **nije** podržan.

## Struktura

| Put | Uloga |
|-----|--------|
| `qnc-app/` | Native egui UI |
| `qnc-host/` | Rust API + SQLite |
| `qnc-client/` | Legacy F4 client, excluded from workspace/product build |
| `qnc-broadcast-player/` | Neutral frame-based player core |
| `qnc-media-ffmpeg/` | FFmpeg media adapter |
| `qnc-player-output/` | Monitor/audio output and telemetry |
| `qnc-player-monitor*/` | Passive monitor projection + bridge |
| `qnc-player-runtime/` | Runtime command/event runner |
| `qnc-player-runner/` | Standalone player smoke/diagnostic runner |
| `seed/` | Host seed (system_seed, keyboard, tab manifeste) |
| `data/` | Runtime SQLite |
| `archive/orphan-broadcast-2026-08-01/` | Old qnc-app broadcast/native player code, read-only reference |

## Host moduli

| Modul | Status |
|-------|--------|
| `project`, `ingest`, `story` | produkt API |
| `media_pool` (Rust) | shared helperi za Ingest/Story |
| design-tools HTTP | API only (nema web UI) |

## v0.5.0 — Service adapter checkpoint

- Media service contracts are introduced as the neutral boundary for media, ASR, search and AI adapters.
- Filmstrip, proxy generation, waveform and duration/probe background paths use the configured media processor boundary.
- Local workstation remains the active target; intranet/internet modes stay future adapter targets.

## v0.4.7.2 — Story stability / background throttle snapshot

- Broadcast/media adapter diff is cleared; `qnc-media-ffmpeg` remains at the locked snapshot.
- Host background workers pause new heavy work while playback is active.
- Filmstrip uses 13 segment-start frames without a synthetic 60s duration fallback.
- Selected marker slots are visibly highlighted in the shared timeline component.

## v0.4.7 — Current native/product model

- Product UI is `qnc-app`; host is API + SQLite only.
- `qnc-client` is legacy F4 code and is excluded from the workspace/product build.
- Story playback goes through the native `PlaybackStack` / broadcast-player path, not legacy host `/api/story/playback/*` routes.

## v0.4.6 — PlaybackStack + carrier timeline checkpoint

- One `PlaybackStack`: timeline is progress-bar projection (`CarrierSync`); click/`step_*` = `CueFrame`; Space = toggle only.
- Probe-authoritative fps/field + yadif; large forward cues respawn (`-ss`) to avoid RGB OOM; stale monitor frames rejected.
- Transport shortcuts (`play_pause`, `step_back_frame`, `step_forward_frame`) only from `seed/keyboard-shortcuts.json`.
- Tag `v0.4.6` is a historical playback checkpoint, not the current version.

## v0.4.5 — runtime protocol path

- `qnc-app` player remote now goes through `qnc-player-runtime`, the same runtime/protocol path used by `qnc-player-runner`.
- `BroadcastPlaybackRequest` carries `initial_frame`, so opening a bounded source can cue the requested frame without app-side transport calls.
- `CueFrame` is a neutral protocol command for frame-position cueing and still-frame presentation.

## v0.4.4 — broadcast player truth

- Aktivni player je modularni stack: `qnc-broadcast-player` -> `qnc-media-ffmpeg` -> `qnc-player-output` -> monitor bridge.
- `qnc-app/src/broadcast/**` i `native_player.rs` vise nisu aktivni kod; premjesteni su u `archive/orphan-broadcast-2026-08-01/`.
- Dual-mono monitor mapiranje je u `qnc-player-output`, ne u UI-u i ne u starom app broadcast folderu.
- Boundary/OUT robusnost je testirana u `qnc-broadcast-player` transport engineu; vanjski follow-up nakon boundary eventa nije dio player corea.
- Broadcast player test skripte ciljaju aktivne crateove, bez `broadcast::` filtera.
- Broadcast player **zakljucan** — bez "odmrznuti broadcast player" ne editirati (vidi `AGENTS.md`).
