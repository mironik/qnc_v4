# QNC — native (v0.4.4)

Aktivna radna kopija: **`qnc-app` + `qnc-host`**. Web UI je u `web-arhive/` (nije produkt).

## Pokretanje

```powershell
cd C:\Users\miron\Projects\qnc_v4
.\run_host.ps1
.\run_app.ps1
```

Host je **samo API** (`/api/...`). Nema `/app` HTML/JS.

LAN: `QNC_BIND_HOST=0.0.0.0` zahtijeva `QNC_TRUSTED_LAN=1`. Internet bez auth/proxy **nije** podržan.

## Struktura

| Put | Uloga |
|-----|--------|
| `qnc-app/` | Native egui UI |
| `qnc-host/` | Rust API + SQLite |
| `qnc-client/` | Native client crate |
| `qnc-broadcast-player/` | Neutral frame-based player core |
| `qnc-media-ffmpeg/` | FFmpeg media adapter |
| `qnc-player-output/` | Monitor/audio output and telemetry |
| `qnc-player-monitor*/` | Passive monitor projection + bridge |
| `qnc-player-runtime/` | Runtime command/event runner |
| `qnc-player-runner/` | Standalone player smoke/diagnostic runner |
| `seed/` | Host seed (system_seed, keyboard, tab manifeste) |
| `data/` | Runtime SQLite |
| `archive/orphan-broadcast-2026-08-01/` | Old qnc-app broadcast/native player code, read-only reference |
| `web-arhive/` | Legacy web `app/` + `plugins/` — samo referenca |

## Host moduli

| Modul | Status |
|-------|--------|
| `project`, `ingest`, `story` | produkt API |
| `media_pool` (Rust) | shared helperi za Ingest/Story |
| design-tools HTTP | API only (nema web UI) |

## v0.4.4 — broadcast player truth

- Aktivni player je modularni stack: `qnc-broadcast-player` -> `qnc-media-ffmpeg` -> `qnc-player-output` -> monitor bridge.
- `qnc-app/src/broadcast/**` i `native_player.rs` vise nisu aktivni kod; premjesteni su u `archive/orphan-broadcast-2026-08-01/`.
- Dual-mono monitor mapiranje je u `qnc-player-output`, ne u UI-u i ne u starom app broadcast folderu.
- Boundary/OUT robusnost je testirana u `qnc-broadcast-player` transport engineu; vanjski follow-up nakon boundary eventa nije dio player corea.
- Broadcast player test skripte ciljaju aktivne crateove, bez `broadcast::` filtera.
- Broadcast player **zakljucan** — bez "odmrznuti broadcast player" ne editirati (vidi `AGENTS.md`).
