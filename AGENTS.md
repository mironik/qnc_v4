# QNC — upute za agenta (obavezno)

Workspace: `C:\Users\miron\Projects\qnc_v4` (native track).

## Produkt

- **`qnc-app`** = native egui UI (jedini UI)
- **`qnc-host`** = Rust API + SQLite (bez web static serve)
- **`seed/`** = host seed JSON (templates, keyboard, tab manifeste)
- Legacy web UI is not part of `qnc_v4`. Do not recreate `web-arhive/`,
  root `app/`, or root `plugins/` as product UI.

## Zabranjeno

- Vraćati `app/` ili `plugins/` na root
- Servirati HTML/JS iz hosta kao produkt UI
- Editirati `qnc-app/src/qnc_timeline.rs` bez izričitog odobrenja (vidi `.cursor/rules/qnc-timeline-freeze.mdc`)
- Editirati **broadcast player** bez izricitog "odmrznuti broadcast player":
  `qnc-broadcast-player/**`, `qnc-media-ffmpeg/**`, `qnc-player-output/**`,
  `qnc-player-monitor/**`, `qnc-player-monitor-bridge/**`, `qnc-player-runtime/**`,
  `qnc-player-runner/**`, `player_remote.rs`, `player_bridge.rs`, `qnc_broadcast_player.rs`,
  `qnc-app/scripts/test-broadcast-player*.ps1` (vidi `.cursor/rules/qnc-broadcast-player-freeze.mdc`)
- `archive/orphan-broadcast-2026-08-01/**` je arhiva starog app broadcast/native player koda.
  Smije se citati za port mapu, ali ne smije se vracati u aktivni `qnc-app/src`.
- Python runtime / FastAPI / pytest

## Arhitektura

```
qnc_v4/
  qnc-app/       native UI
  qnc-host/      API
  qnc-client/    client crate
  seed/          host-owned JSON
  data/          runtime SQLite
  archive/       read-only historical reference
```

## Pokretanje

```powershell
.\run_host.ps1
.\run_app.ps1
```

Test: `.\test.ps1` (API, bez web asset testova).

## DB-first

Workflow stanje živi u SQLite preko Rust API-ja. UI je projekcija — nikad vlasnik.
