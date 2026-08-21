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
  qnc-client/    legacy F4 client, excluded from workspace/product build
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

## Deployment i mediji

- Prvo se stabilizira lokalni workstation mode: `qnc-app` + `qnc-host` +
  SQLite + lokalni/intranet medijski artefakti. Novi intranet/internet slojevi
  ne uvode se dok osnovni lokalni tok nije stabilan.
- Cilj nije više različitih aplikacija, nego jedna workstation aplikacija koja
  zna raditi u različitim okruženjima: lokalni laptop/radna stanica,
  TV intranet s centralnim ingestom/NAS-om, i kasnije internet/tester mode.
- Mediji nikad nisu na internetu. Internet/tester mode smije prenositi projekt,
  bazu/metapodatke, edit decision, korisničko stanje i lake snapshote, ali
  originalni media, proxy, filmstrip i waveform ostaju lokalno kod korisnika
  ili u intranet storageu koji vidi lokalna radna stanica/media resolver.
- Fizička lokacija medija nije workflow istina. UI, Story, timeline, export i
  player ne smiju pretpostavljati lokalni Windows path kao poslovno pravilo.
  Do medija se dolazi kroz neutralni resolver/host contract.
- Montaža se bazira na virtualnim kadrovima: `clip_id`/source identity +
  `source_in`/`source_out` frameovi + source FPS/timebase. To mora ostati isto
  bez obzira je li fizički fajl na laptopu, intranet serveru/NAS-u ili iza
  budućeg lokalnog media agenta.
- Intranet security/auth je svjesno odgođen kao poseban neutralni modul. Ne
  miješati auth/security u Project, Ingest, Story, player, timeline ili worker
  popravke dok se osnovni funkcionalni tok stabilizira.

## Jedinstveni model

- Problem se ne smije popravljati samo u jednom tabu, formi ili panelu. Ako
  isti koncept postoji u Source timelineu, Segment timelineu, Program/playlist
  overviewu, Storyju, Media Assistu ili Ingestu, rješenje mora ići kroz
  zajednički model/contract i vrijediti za sve opcije.
- "Riješeno" znači da je riješeno na cijelom zajedničkom toku: isti timeline
  layer contract, isti broadcast-player input/progress princip i isti
  frame-based model. Zabranjeni su lokalni workaroundi koji poprave samo
  jedan prikaz, a ostave drugi na starom modelu.
- Nijedan FPS ne smije biti zakljucan kao preset, fallback ili implicitno
  pravilo play komponente. Ako probe/metapodaci sourcea vrate 25, 30, 50, 60,
  59.94 ili drugi valjan rate, playback koristi upravo taj probed timebase.
  Zabranjeno je samo izmisljanje FPS-a kad probe nije spreman ili valjan.
- Forme su pasivne komponente: prikaz, unos i emitiranje neutralnih intentova.
  Kompletan aktivni kod dolazi iz komponenti/modula koji posjeduju taj
  contract. Dok se rješava modul ili komponenta, promjene ostaju unutar tog
  modula / komponente i njezinog eksplicitnog contracta; ne dirati druge
  komponente kao usputni workaround.
- Sve komponente moraju biti neutralne i samostalne: ne smiju čitati stanje iz
  forme kao poslovno pravilo, ne smiju pretpostavljati aktivni tab i ne smiju
  uvoditi lokalne fallback modele koji zaobilaze njihov contract.
- Play ritam, streaming tick, frame catch-up i pause/play lifecycle vlasništvo
  su play/broadcast-player komponente. Source timeline, segment timeline,
  program/playlist prikaz, Story i Media Assist smiju samo slati neutralne
  intentove i koristiti tu istu komponentu univerzalno.
- FPS za playback određuje probe/metapodaci sourcea. Play komponenta ne smije
  koristiti project/export FPS, hardkodirani FPS ili lokalni fallback kad
  otvara source input; ako probe FPS nije spreman, open/play se odbija kao
  nespreman.
- `QncTimeline` je jedan neutralni timeline core. Source, Segment i Program ne
  smiju imati paralelne timeline implementacije; smiju imati samo adaptere koji
  pune isti layer contract i aktiviraju/deaktiviraju potrebne layere.
- Svi timeline elementi su virtualni kadrovi po contractu. Razlika izmedu
  source virtuala, segmenta, pokrivalice ili drugog prikaza smije biti samo
  `kind`/hint/workflow naziv, ne drugi aktivni playback model.
- Svaki virtualni kadar ima vlastiti `source_in`/`source_out`. Segmenti takoder
  imaju svoj IN/OUT i tretiraju se kao virtualni kadrovi u EDL/playlist
  contractu, iako se ne prikazuju u Virtual tabu nego u Segment prikazu.
- Timeline, Segment i Program UI su pasivni: prikazuju virtualne rangeove i
  emitiraju frame/selection intent. Playlist/EDL builder pretvara te virtualne
  kadrove u jedan broadcast-player input; UI elementi sami ne playaju media.
