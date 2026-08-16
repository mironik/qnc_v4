# QNC — upute za agenta (obavezno)

Workspace: `C:\Users\miron\Projects\qnc_v4` (native track).

## Produkt

- **`qnc-app`** = native egui UI (jedini produkt UI; forme su pasivni layout hostovi)
- **`qnc-host`** = Rust API + SQLite (bez web static serve)
- **`qnc-broadcast-player`** + **`qnc-media-ffmpeg`** + **`qnc-player-*`** =
  neutralni frame-based playback/decode/runtime stack
- **`qnc-client`** = legacy/paralelni client crate; nije produkt UI owner
- **`seed/`** = host seed JSON (templates, keyboard, tab manifeste)
- Legacy web UI is not part of `qnc_v4`. Do not recreate `web-arhive/`,
  root `app/`, or root `plugins/` as product UI.

## Zabranjeno

- Vraćati `app/` ili `plugins/` na root
- Servirati HTML/JS iz hosta kao produkt UI
- Editirati `qnc-app/src/qnc_timeline.rs` bez izričitog odobrenja (vidi `.cursor/rules/qnc-timeline-freeze.mdc`)
- Story Segment/Wrap M-marker i marker-slot model je obavezno pravilo
  (vidi `.cursor/rules/qnc-story-segment-timeline.mdc`)
- Ingest `Uvezi` ugovor je zaključan: UI šalje neutralni ingest import
  command; `/api/ingest/import` kreira queue u SQLite; prazan `clip_ids`
  znači da host čita `ingest_assets.selected != 0`; host postavlja
  `import_status='queued'`, upisuje `ingest_jobs`, vraća svježi
  `ingest.state`, i tek tada workflow smije prijeći s Ingesta na sljedeću
  formu. Stvarni import/proxy nastavlja u pozadini. Ne popravljati ovaj tok
  izmjenama u Story, MA, segment timelineu, qnc_timelineu, broadcast playeru,
  Projectu ili Shellu bez izričitog odmrzavanja tog područja.
- Editirati **broadcast player** bez izricitog "odmrznuti broadcast player":
  `qnc-broadcast-player/**`, `qnc-media-ffmpeg/**`, `qnc-player-output/**`,
  `qnc-player-monitor/**`, `qnc-player-monitor-bridge/**`, `qnc-player-runtime/**`,
  `qnc-player-runner/**`, `player_remote.rs`, `player_bridge.rs`, `qnc_broadcast_player.rs`,
  `qnc-app/scripts/test-broadcast-player*.ps1` (vidi `.cursor/rules/qnc-broadcast-player-freeze.mdc`)
- `archive/orphan-broadcast-2026-08-01/**` je arhiva starog app broadcast/native player koda.
  Smije se citati za port mapu, ali ne smije se vracati u aktivni `qnc-app/src`.
- Python runtime / FastAPI / pytest

## Zaključane forme

- **Project forma je zaključana.** Ne editirati `qnc-app/src/project/**`,
  `qnc-app/src/project_pts.rs`, `qnc-host/src/project/**`, project seed,
  project workflow, project settings ili project init tok bez izričitog
  "odmrzni project". Project forma je pasivna projekcija Project Registry /
  Project Settings backend ugovora; ne smije postati vlasnik workflowa,
  ingest queuea, playbacka ili statusa drugih komponenti.
- **Ingest forma je zaključana.** Ne editirati `qnc-app/src/ingest/**`,
  `qnc-app/src/components/source_import_*.rs`, `qnc-host/src/ingest/**` ili
  `qnc-host/src/ingest_import/**` bez izričitog "odmrzni ingest".
  `Uvezi` ostaje DB-first queue ugovor: UI salje neutralni import command,
  host cita SQLite odabir, zapisuje queue/status i vraca snapshot; workflow
  ide dalje tek nakon uspjesnog queue snapshota, dok import nastavlja u
  pozadini.
- Project i Ingest se ne popravljaju kroz Story, MA, segment timeline,
  qnc_timeline, broadcast player ili Shell. Ako kvar izgleda povezano, prvo
  dokazati neutralni API/DB ugovor i traziti izricito odmrzavanje drugog
  podrucja.

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

Test: `.\test.ps1` (API + qnc-app unit smoke, bez web asset testova).

## DB-first

Workflow stanje živi u SQLite preko Rust API-ja. UI je projekcija — nikad vlasnik.
