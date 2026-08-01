# QNC v4 Broadcast Player Port Map

Datum: 2026-08-01

## Aktivna istina

Aktivni broadcast player nije `qnc-app/src/broadcast/**`.

Aktivni runtime put je:

```text
qnc-app/src/player_remote.rs
  -> qnc-broadcast-player
  -> qnc-media-ffmpeg
  -> qnc-player-output
  -> qnc-player-monitor-bridge
  -> qnc-player-monitor
```

`qnc-app/src/qnc_broadcast_player.rs` je egui wrapper/monitor host za aktivni
player remote. Ne sadrzi media decode, probe, IN/OUT/M edit logiku ni form
ownership.

## Arhiva

Stari app broadcast kod premjesten je u:

```text
archive/orphan-broadcast-2026-08-01/
```

Sadrzaj:

- `qnc-app-src/broadcast/**`
- `qnc-app-src/native_player.rs`
- `qnc-app-src-story/program_builder.rs`
- stare `qnc-app/scripts/test-broadcast-player*.ps1`

Arhiva je samo referenca. Ne vracati je u aktivni `qnc-app/src`.

## Portano

| Tema iz starog koda | Aktivno mjesto | Stanje |
|---|---|---|
| Frame-based transport / boundary | `qnc-broadcast-player/src/transport_engine.rs` | Aktivno, testirano |
| OUT/boundary mora biti prezentiran prije pause eventa | `qnc-broadcast-player/src/transport_engine.rs` | Aktivno, testirano |
| Delayed tick ne smije preskociti boundary frame | `qnc-broadcast-player/src/transport_engine.rs` | Aktivno, testirano |
| FFmpeg decode/cache | `qnc-media-ffmpeg/src/lib.rs` | Aktivno |
| Hardware decode policy | `qnc-host/src/hardware_profile/**`, `qnc-app/src/player_remote.rs`, `qnc-media-ffmpeg` | Aktivno |
| A/V sync telemetry | `qnc-player-output/src/lib.rs` | Aktivno |
| Dropped frame / buffer events | `qnc-player-output/src/lib.rs` | Aktivno |
| Dual-mono monitor map | `qnc-player-output/src/lib.rs` | Aktivno, testirano |
| Passive monitor projection | `qnc-player-monitor/**`, `qnc-player-monitor-bridge/**` | Aktivno |
| Standalone runner smoke tests | `qnc-player-runner/**` | Aktivno |

## Ne portati u player core

Ovo je namjerno izvan `qnc-broadcast-player` corea:

- Story/MA/Ingest/form semantika
- IN/OUT/M edit odluke
- program graph kao edit/timeline model
- filmstrip
- UI timeline
- DB ownership
- project/workflow ownership

Player core izvrsava frame-based playback request i emitira neutralne evente.
Sve sto je edit odluka, playlist/EDL priprema ili UI prikaz ostaje izvan corea.

## Test istina

Aktivna test skripta:

```powershell
pwsh -File qnc-app\scripts\test-broadcast-player.ps1
```

Live smoke:

```powershell
pwsh -File qnc-app\scripts\test-broadcast-player-live.ps1
```

Stare skripte koje su filtrirale `broadcast::` pokretale su 0 aktivnih testova
i zato su arhivirane.
