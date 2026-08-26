# QNC — native (v0.5.1.1)

Aktivna radna kopija: **`qnc-app` + `qnc-host`**. Web UI nije dio `qnc_v4`
produkta i legacy `web-arhive/` folder nije prisutan u ovom treeu.

## Pokretanje

```powershell
cd C:\Users\miron\Projects\qnc_v4
.\run_host.ps1
.\run_app.ps1
```

Host je **API + SQLite koordinator** (`/api/...`). Nema `/app` HTML/JS.

`run_host.ps1` automatski pokrece lokalni `qnc-worker` proces za teske artifact
poslove (`proxy_generate`, `filmstrip`). Host ne izvršava artifact poslove
lokalno u svom procesu.

Default capabilities su `proxy_generate,filmstrip`; `QNC_WORKER_CAPABILITIES`
moze zadati drugi popis za specijalizirane workere. Isti `qnc-worker` se koristi
na lokalnoj radnoj stanici i na intranet worker stroju; placement se detektira
iz `QNC_HOST_URL` / `--host-url`, a po potrebi se rucno zakljucava preko
`QNC_WORKER_PLACEMENT` / `--placement`.

LAN: `QNC_BIND_HOST=0.0.0.0` zahtijeva `QNC_TRUSTED_LAN=1`. Internet bez auth/proxy **nije** podržan.

Media access ide kroz isti integration gateway model u svim okruzenjima. Lokalni
filesystem/NAS adapter i enterprise migracijski proxy prema postojecem
MAM/ingest/archive sustavu su razlicite implementacije istog ugovora. Taj sloj
je neinvazivan i read-through po defaultu; postojeća TV infrastruktura ostaje
vlasnik svojih baza, storage pravila i produkcijskog toka.

## Deployment konfiguracija

`config.toml` definira ciljnu topologiju, ali ne uvodi drugi kodni put:

```toml
deployment = "single_workstation"

[integration.gateway]
kind = "local_fs"
read_only = true

# enterprise_proxy primjer:
# endpoint = "http://mam-gateway.local/qnc"
#
# [integration.gateway.routes]
# playback_proxy = "/media/{access}/{project_id}/{clip_id}"
# original_master = "/media/{access}/{project_id}/{clip_id}"
```

Vrijednosti za `deployment`:

- `single_workstation` — novinar/laptop, app + host + worker na istom stroju.
- `shared_worker` — mala produkcija, laptopi montiraju, jedan jaci stroj radi background poslove.
- `enterprise_gateway` — TV intranet, QNC ide kroz gateway/proxy prema postojecim sustavima.
- `internet_project` — projekti/metapodaci online, mediji ostaju lokalno kod korisnika.

Vrijednosti za `integration.gateway.kind`:

- `local_fs` — lokalni disk ili lokalno montiran medij.
- `shared_fs` — zajednicki NAS/share za vise radnih stanica.
- `enterprise_proxy` — neinvazivni proxy/gateway prema MAM/ingest/archive sustavu.

Manualni override je dopusten za deployment/proizvodne topologije:
`QNC_DEPLOYMENT` i `QNC_INTEGRATION_GATEWAY_KIND`.

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

## v0.5.1.1 — Story segment UI / catalog checkpoint

- Segment panel header now shows active segment duration, playlist playhead and total story duration in one right-aligned timing group.
- Total duration is emphasized as `Trajanje`; playhead and segment values use their timeline colors.
- Cover/B-roll virtual shots are separated from normal short virtual shots through `cover_shots`; source-root shots remain All-only and segments remain the Segment catalog.
- Segment deletion is soft-deactivated for the Segment catalog while active program playback uses only active parts.

## v0.5.1 — Sync cover checkpoint

- Story Sync cover flow adds a standalone Sync button in the segment panel.
- Sync play starts only after Source IN, uses one broadcast playlist input, and keeps frame-only source/program mapping.
- Sync OUT creates only the marker; Enter commits the captured cover into the Sync-created slot.
- Shift+B remains the normal cover command and no longer falls back to the first empty slot when no valid slot is selected.
- Source-root shots in All do not auto-arm Sync; short virtual shots in Virtual can arm from their existing IN.

## v0.5.0.1 — Stability backup

- JobService heartbeat accepts every leased artifact job, including `media_probe`.
- Playback media resolve persists probed `fps/duration/source_timebase` back to SQLite.
- Story/MA source selection waits for resolver metadata when snapshots are incomplete.
- Cover creation uses the selected source/virtual range by default; manual IN/OUT are overrides.

## v0.5.0 — Service adapter checkpoint

- Media service contracts are introduced as the neutral boundary for media, ASR, search and AI adapters.
- Filmstrip, proxy generation, waveform and duration/probe background paths use the configured media processor boundary.
- `proxy_generate` and `filmstrip` now have explicit ownership through JobService. One `qnc-worker` binary self-detects local workstation vs intranet shared-media placement and allows a manual placement override.
- Integration is represented as one read-through media gateway scaffold for local filesystem/NAS and enterprise MAM/ingest/archive proxy adapters, not direct writes into existing systems.
- Deployment is now a typed runtime config: `single_workstation`, `shared_worker`, `enterprise_gateway`, `internet_project`.
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
