# QNC Playback Engine

Status: v0.4.4, aktivni modularni broadcast player.

## Aktivni Put

```text
qnc-app/src/player_remote.rs
  -> qnc-broadcast-player
  -> qnc-media-ffmpeg
  -> qnc-player-output
  -> qnc-player-monitor-bridge
  -> qnc-player-monitor
```

`qnc-broadcast-player` je neutralni frame-based executor. On ne zna za formu,
workflow, Story, Ingest, timeline, filmstrip, project ni DB. Dobiva source
runtime/playback request i vraca neutralne evente:

- `CarrierPositionChanged`
- `TransportStatusChanged`
- `PlaybackBoundaryReached`
- `FramePresented`
- `DroppedFrame`
- `AudioRuntimeChanged`
- `AVSyncWarning`
- `BufferStateChanged`
- `PlaybackError`

## Granice

Player core smije imati:

- frame/timebase modele
- source runtime snapshot
- transport engine
- execution range kao granicu izvrsenja zahtjeva
- adapter ugovore za source open, video decode, audio output i frame presenter

Player core ne smije imati:

- UI timeline
- filmstrip
- IN/OUT/M edit semantiku
- Story/MA/Ingest grane
- project/workflow odluke
- DB ownership
- program graph kao montazni model

## Adapteri

- `qnc-media-ffmpeg` radi probe/decode/cache i optional hardware decode policy.
- `qnc-player-output` radi output telemetry, A/V sync warninge, dropped-frame
  evente i dual-mono monitor mapiranje.
- `qnc-player-monitor` i `qnc-player-monitor-bridge` su pasivne projekcije
  event/frame podataka.
- `qnc-player-runtime` i `qnc-player-runner` daju samostalni command/event
  runtime i realne smoke testove.

## Arhiva

Stari `qnc-app/src/broadcast/**`, `native_player.rs`, orphan
`story/program_builder.rs` i stare player test skripte premjesteni su u:

```text
archive/orphan-broadcast-2026-08-01/
```

Ta arhiva nije produktni kod.
