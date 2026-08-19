# QNC Playback Engine

Status: v0.4.6, aktivni modularni broadcast player.

## Aktivni Put

```text
qnc-app/src/player_remote.rs
  -> qnc-player-runtime
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

## Story/program playback rule

Story/program playback uses a streaming playlist-input model.

- The playlist input is treated as one playable input, with video and A1/A2
  resolved together per playlist item.
- UI timeline ranges, covers and slots only build the playlist input; they are
  not active playback elements.
- Playback must not decode the whole playlist at open or at play start.
- The adapter may prepare/decode only the current item and a small forward
  streaming window. Later items are opened lazily when the transport reaches
  them.
- Broadcast playback must not silently drop video frames or audio packets. A
  late decode is an underrun to prevent with streaming prebuffer, or to report
  explicitly; it is not solved by skipping frames.

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
