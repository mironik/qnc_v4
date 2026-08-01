# QNC playback engine (Rust, proxy-first)

**Status:** Full Kodak native pump — 2026-07-21  
**Povezano:** [qnc-step-plan.md](qnc-step-plan.md), [qstory-editorial-playlist.md](qstory-editorial-playlist.md), [qnc-wrap-timeline-model.md](qnc-wrap-timeline-model.md)

## Product path (canonical)

**Product decode owner = `BroadcastPlaybackPump` + `FfmpegBroadcastBackend`**,
wired through `BroadcastEngine` → `PlayerRemote` → `QncBroadcastPlayer`.

`QncBroadcastPlayer` is a **neutral component**: Open / Play / Pause / Seek /
Stop + present RGBA/PCM. It does **not** know Story, Wrap, Ingest, or filmstrip.
Forms build a finished `UniversalTimelineSpec` (or use the default single-shot
program from ffprobe) and send it in. Filmstrip belongs to **timeline UI** and
is stripped on open — the player never decodes or paints it.

```text
Form (any)
  → PlayerCommand / BroadcastPlayerOpenRequest (+ optional program spec)
  → BroadcastEngine (worker thread)
  → BroadcastPlaybackPump + FfmpegBroadcastBackend
  → composite RGBA + mix A1–A4 → egui texture / rodio
  → PlayerEvent RX (bounded fan-out)
```

- Open probes media via **ffprobe** into a rational `Timebase`.
- Default program: BaseVideo + A1 (+ reserved A2 silence), no filmstrip.
- Layered programs (overlays, blank/no base, markers) are built by forms and
  passed as `UniversalTimelineSpec` — not interpreted as Story/Wrap inside the player.
- Pause holds last frame; Stop tears down.
- `NativePlayer` is **legacy** (not on the UI path).

## Native broadcast smjer — 2026-07-18

Ovaj dokument opisuje stariji `qnc-host` MVP/proxy-first playback. Za native
`qnc-app` broadcast motor vrijedi širi QNC/Kodak model:

1. **Celulozna traka = transparentni timecode carrier** — nije vizualni sloj,
   nego podloga za frame identity, source FPS, PTS i source/project mapping.
2. **Svi slojevi se registriraju na carrier** — filmstrip, base video, audio
   1–4, overlay/pokrivalice 1..n i marker/effect layer.
3. **Filmstrip nije playback media** — služi za UI/orijentaciju; player nikad ne
   dekodira filmstrip kao video izvor. Vizualno može biti underlay ispod
   transparentne celulozne/timecode trake.
4. **Clock nije audio sink i nije video worker** — program/reference clock prati
   carrier; audio/video/overlay/effect rendereri su potrošači tog clocka.
5. **Klip bez audio tracka je validan** — audio lane postaje explicit silence,
   ne clock failure.
6. **M marker je effect layer** — sada marker/cut event; isti princip kasnije
   pokriva dissolve i druge efekte.
7. **Z os = prioritet vizualnih/effect slojeva** — filmstrip je ispod
   carrier-a, base video je osnovni sloj, overlay/pokrivalice su iznad njega, a
   marker/effect slojevi su najviši prioritet. **Timecode/celluloid carrier i
   audio nisu dio Z ordera**: carrier je koordinatni sustav, audio kanali su
   laneovi na istom vremenskom busu. Renderer/decode plan ne koristi goli `i32`
   redoslijed nego `ZPriority`, ali samo za vizualni/effect prioritet. Audio
   decode request nema Z polje i smije ovisiti samo o carrier frame/PTS i audio
   kanalu.
8. **Base video nije obavezan** — u OFF/VO montaži program može imati samo
   celuloznu traku/timecode + audio laneove. Video stack nastaje tek kad
   korisnik doda pokrivalice/overlay ili eksplicitni base video.
9. **OFF/VO i pokrivalice imaju fiksnu audio semantiku** — OFF/VO/ton ide na
   A1 i može imati vlastiti mix: unity, smanjenje nivoa u dB ili mute.
   Pokrivalica je uvijek overlay video sloj, a njezin audio ide na A2 s
   istim carrier frame rangeom kao pokrivalica. A2 nikad ne zamjenjuje A1 i ne
   ulazi u vizualnu Z os. Svaka pojedinačna pokrivalica može imati vlastiti A2
   mix: unity, smanjenje nivoa u dB ili mute.

Implementacijski početak je u `qnc-app/src/broadcast/`: `CelluloidTrack`,
`UniversalTimelineSpec`, `BroadcastProgramGraph`, `BroadcastRenderPlan`,
`BroadcastFrameScheduler`, `ProgramLayer*`, `BroadcastMasterClock`.

Native tok mora ostati:

```text
DB virtual shot / editorial truth
  → source media probe (ffprobe rational rate → source_timebase)
  → BroadcastPlaybackSource (asset metadata + DB frame range)
  → UniversalTimelineSpec
  → BroadcastProgramGraph
  → BroadcastRenderPlan
  → BroadcastFrameScheduler
  → FrameDecodePlan
  → ResolvedFrameDecodePlan (virtual shot/clip → local path or URL asset)
  → AudioMixPlan (audio only, no visual Z)
  → VideoCompositePlan (video only, visual Z)
  → BroadcastPresentationPlan (one carrier frame boundary)
  → FrameDecodeBatch / BroadcastPresentationBatch / BroadcastDecodeWorker
  → BroadcastRuntimeDriver (master-clock tick + queue/diagnostic coordination)
  → BroadcastPlayoutFrame
  → decoder/render backend
```

UI filmstrip završava u `TimelineUnderlay`; ne smije prijeći u decoder input.
`source_timebase` mora doći iz source/proxy media probea. `FfprobeMediaProbe`
čita `avg_frame_rate` / `r_frame_rate` kao racionalni `num/den`; taj racionalni
FPS ide u `Timebase`. Float FPS i UI defaulti nisu istina vremena.
`BroadcastPlaybackSource::from_media_asset` je ulaz za source virtual kadar:
uzima probed `BroadcastMediaAsset` i DB frame range. Ako stari API još šalje
IN/OUT u sekundama, one se odmah pretvaraju u frame range pomoću asset
`source_timebase`-a; dalje kroz engine putuju samo frameovi.
`BroadcastHostSourceRef` je adapter za postojeći Story/API oblik podataka:
gradi unprobed proxy asset seed iz project/shot/clip identiteta i URL-a, zatim
nakon ffprobe izvještaja stvara `BroadcastPlaybackSource`. Legacy `shot.fps`
nije ulaz u taj put.
`FrameDecodePlan` je jedini dozvoljeni input za media backend. Sadrži carrier
frame, PTS sekunde, aktivne video slojeve, audio busove i marker/effect evente.
`pts_sec` je program/timeline vrijeme, a svaki audio/video request dodatno nosi
`media_seek_sec`, tj. vrijeme na stvarnom media assetu. To su namjerno odvojene
vrijednosti: source virtual kadar može početi na source frameu 100, gdje je
program PTS 0.0s, ali media seek nije 0.0s.
Prije stvarnog decodea `ResolvedFrameDecodePlan` mapira svaki `VirtualShot`
source na eksplicitni `BroadcastMediaAsset`: lokalni path, LAN/HTTP URL ili
host-stream URL. Blank video i silence audio ostaju eksplicitni non-media
sourceovi. Resolver nema fallback na filmstrip i javlja grešku ako asset ne
može poslužiti traženi video/audio stream.
Audio decode request dodatno nosi egzaktni 48 kHz `AudioSampleSpan` izveden iz
carrier framea i `source_timebase`-a. Broj sampleova po frameu nije hardkodiran:
dolazi iz FPS-a source datoteke, a na fractional rateovima (29.97/59.94) sample
granice se računaju integer frame-boundary metodom, bez driftanja po floating
sekundama.
Source FPS nije ograničen na 25/50. Validni su svi probed racionalni rateovi
koje source stvarno nosi, npr. 24, 25, 30, 50, 60, 30000/1001 ili 60000/1001;
svaki `pts_sec`, `media_seek_sec` i audio sample span mora se izvesti iz tog
`source_timebase`-a.
Svaki `DecodedProgramFrame` koji backend vrati mora proći validaciju protiv tog
plana: isti carrier frame, isti PTS i samo traženi audio/video slojevi. Time
se greške tipa ubrzan video, audio izvan synca ili krivi layer hvataju na
engine granici, ne u UI-ju.
Konkretni native payload format za backend je također eksplicitan:
`BroadcastVideoPayload` je RGBA/BGRA frame s width/height/stride/pixel
formatom, BT.709/BT.601/SRGB color spaceom i scan modeom; `BroadcastAudioPayload`
je isključivo 48 kHz interleaved f32 PCM block vezan na isti `AudioSampleSpan`.
Backend može interno koristiti FFmpeg/GStreamer, ali na engine granici mora
vratiti ove oblike ili failati.
`FfmpegBroadcastBackend` je prvi native backend adapter: prima samo resolved
plan, gradi FFmpeg komande koje seekaju po `media_seek_sec`, ne po program
`pts_sec`, dekodira raw RGBA/BGRA video i 48 kHz f32 PCM audio, zatim validira
frame/PTS/media seek/sample span/payload contract prije nego payload uđe u
queueove.
`AudioMixPlan` se izvodi iz istog frame decode plana i odlučuje što se stvarno
čuje na A1/A2/A3/A4: role, gain, mute i source. Audio mix plan provjerava da su
svi audio inputi na istom carrier frameu i PTS-u; ne koristi vizualni Z prioritet.
`VideoCompositePlan` se također izvodi iz istog frame decode plana i odlučuje
koji se vizualni slojevi compositiraju za taj carrier frame. Koristi samo
vizualni `ZPriority`; audio i timecode carrier nisu dio plana. OFF/VO frame bez
videa je validan plan s praznim video stackom, a pokrivalica može biti jedini
video sloj dok je aktivna.
`BroadcastPresentationPlan` spaja audio mix, video composite i marker/effect
evente u jedan frame-level boundary. Player/UI smije čitati taj plan, ali ne
smije sam ponovno odlučivati koji audio/video slojevi vrijede za frame.
`BroadcastPresentationBatch` radi isto za lookahead prozor: niz carrier frameova
u stabilnom redoslijedu za player/renderer. Batch ne mijenja clock; samo
pakira odluke već izvedene iz carrier-a.
`PresentationPlanQueue` je runtime buffer za presentation batcheve. Kao i video
queue, ne posjeduje clock; player traži najnoviji spremni plan za trenutni
carrier frame.
`AudioFrameQueue` je isti princip za dekodirani audio payload: audio izlaz može
uzeti najnoviji spremni audio frame za master frame, ali ne smije pogurati ili
usporiti program clock. Video i audio queueovi su potrošači carrier-a.
`BroadcastPlayoutFrame` je zadnji player-facing odabir za master frame: spaja
presentation plan i decoded video payload. Ako video nije potreban, frame je
audio-only; ako payload kasni, stanje je eksplicitno `Missing` ili
`HoldPrevious`, bez skrivene promjene clocka. `PlayoutReadiness` razlikuje
`Clean`, `AudioOnly`, `PresentationHold`, `VideoHold` i `VideoMissing`, što je
osnova za kasniji UI/debug prikaz realnog uzroka trzanja.
`BroadcastPlayoutDiagnostics` dodaje read-only health snapshot: presentation
queue span, video queue span i konkretan `PlayoutProblem` (`NoPresentation`,
`PresentationBehind`, `VideoMissing`, `VideoBehind`). To je namijenjeno za
debug overlay i logiranje bez utjecaja na clock.
`BroadcastRuntimeDriver` je tanak runtime tick iznad ovih dijelova. Za trenutni
master frame iz session clocka priprema `FrameDecodeBatch`, iz njega izvodi
`BroadcastPresentationBatch`, puni presentation queue i vraća `BroadcastPlayoutFrame`
zajedno s dijagnostikom. Driver ne dekodira i ne posjeduje clock; realni media
backend samo konzumira decode batch i puni payload queueove.
`NullBroadcastBackend` postoji kao testna zaštita: odbija svaki pokušaj da
filmstrip uđe u decoder path.

`BroadcastDecodeWorker` nema clock. Dobiva `FrameDecodeBatch` lookahead prozor,
dekodira ga backendom i puni `VideoFrameQueue` i/ili `AudioFrameQueue`.
`BroadcastResolvedDecodeWorker` je put za stvarni native backend: svaki frame iz
batcha prvo resolvea u `ResolvedFrameDecodePlan`, zatim ga šalje
`BroadcastResolvedDecodeBackend` implementaciji kao što je `FfmpegBroadcastBackend`.
Time se zadržava pravilo: program/celluloid clock vodi playback, decode worker
samo puni queueove.
`BroadcastPlaybackPump` je prvi player-facing bridge iznad toga: poziva
`BroadcastRuntimeDriver`, dobiva master frame, decode batch i presentation
batch, zatim resolved decode workerom puni queueove i vraća finalni
`BroadcastPlayoutFrame` s dijagnostikom. Pump je namjerno odvojen od Story UI-ja
dok FFmpeg backend ne dobije persistent decode worker; inače bismo vratili
spor frame-per-command playback u korisnički path.

## Načela

1. **Playback živi u `qnc-host`** (FFmpeg decode/mix). Browser `<video>` / Web Audio nisu montažni motor.
2. **Proxy-first** — timeline scrub/play ide na H.264 (ili XDCAM) proxy u projektnom `proxy/` folderu.
   H.264 raster = **ffprobe izvora** (bez hardcodirane 720/1080 skale). XDCAM HD422 profil i dalje 1920×1080 kad trebate.
3. **Original** (XDCAM / XAVC / MXF / MP4 …) je **reserved** — export / “open source” kasnije; nije wired u `PlaybackSession`.
4. **SQLite + playlist** = istina vremena; klijent samo prikazuje `mixed_audio_url` + `preview_frame_url`.

## Medijski izvor

| Kind | MVP | Namjena |
|------|-----|---------|
| `proxy` | **default** | Play, scrub, mix, preview frame |
| `original` | export / source-open | XML export + `GET /api/media-pool/original` |

Rust API:

```text
resolve_play_media(project_id, clip_id) → PlayMedia { path, kind: Proxy }
resolve_original_media(project_id, clip_id) → PlayMedia { path, kind: Original }
```

Ako proxy fajl ne postoji → greška **`proxy_missing`** (nema tihog fallbacka na original).  
Ako original ne postoji → greška **`original_missing`**.

## Clock i sloj

- Clock: `virtual_sec`, `playing`, `paused` (seek / pause / stop preko API-ja).
- Active layer: `part` | `cover` | `none` (+ `video_blank` za OFF izvan cover slotа).
- Segment resolve: `part_id` / `cover_id` iz SQLite — klijent ne šalje IN/OUT frameove kao istinu.

## Audio bus (Kodak)

Sada fiksno **2** kanala (perforacije):

| Index | Role | Semantika |
|-------|------|-----------|
| 0 | A1 | Kostur (izjava / off audio) |
| 1 | A2 | Pokrivanje (cover slot) |

Mix: OFF → samo A1; TON + cover slot → A1 + A2.

**Kasnije (ne u ovom milestonu):** `project.settings.audio.max_channels` (1–4) širi bus; UI u Project formi.

## Output surface (HTTP)

| Endpoint | Sadržaj |
|----------|---------|
| `POST /api/story/playback/start\|seek\|pause\|stop` | Sesija |
| `GET /api/story/playback/state` | `active` + URL-ovi |
| `GET /api/story/playback/audio` | Mixed A1 (+ A2) AAC/M4A |
| `GET /api/story/playback/frame` | Preview JPEG |

Istina sinka: server clock + `virtual_sec`. Klijent ne miješa A/V lokalno.

## Tok

```text
Original (MXF/XAVC/…) → ingest proxy → resolve_play_media(Proxy)
  → PlaybackSession → FFmpeg slice/mix
  → mixed_audio_url + preview_frame_url
```

## Izvan scopea (MVP)

- Native `qnc-client` window (F4)
- Direktni play originala
- 3.–4. audio kanal iz project settings
- Preimenovanje `kodak-timeline` → `qnc-timeline`
