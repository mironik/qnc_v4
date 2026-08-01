# QNC — native (v0.4.0)

Aktivna radna kopija: **`qnc-app` + `qnc-host`**. Web UI je u `web-arhive/` (nije produkt).

## Pokretanje

```powershell
cd C:\Users\miron\Projects\QNC
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
| `seed/` | Host seed (system_seed, keyboard, tab manifeste) |
| `data/` | Runtime SQLite |
| `web-arhive/` | Legacy web `app/` + `plugins/` — samo referenca |

## Host moduli

| Modul | Status |
|-------|--------|
| `project`, `ingest`, `story` | produkt API |
| `media_pool` (Rust) | shared helperi za Ingest/Story |
| design-tools HTTP | API only (nema web UI) |

## v0.4.0 — broadcast player

- Live ffmpeg preview engine (A/V lockstep, soft EOS → rewind na virtual-clip IN)
- Dual mono monitor: **A1 → L**, **A2 → R**
- Source-first open (video+audio / video-only / audio-only)
- `qnc-timeline`: playhead iznad filmstrip-a; carrier nije vidljivi žuti sloj
- Broadcast player **zaključan** — bez „odmrznuti broadcast player” ne editirati (vidi `AGENTS.md`)
