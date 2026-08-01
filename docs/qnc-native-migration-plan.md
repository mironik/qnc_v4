# QNC — usklađeni plan native migracije

**Status:** prijedlog · **bez koda** · 2026-07-17  
**Workspace (kanonski za ovaj track):** `C:\Users\miron\Projects\QNC`  
**Ulazi:** [qnc-native-rust-app-plan.md](qnc-native-rust-app-plan.md), [qnc-client-server.md](qnc-client-server.md), [qnc-playback-engine.md](qnc-playback-engine.md), [architecture-db-first.md](architecture-db-first.md)

Ovaj doc **zatvara napetosti** između native UI plana, deployment profila i playback ownershipa.  
Kad se odobri: implementacija ide po §6; web UI ostaje legacy do parity.

---

## 1. Cilj (jedna rečenica)

Jedan Rust desktop klijent `qnc-app` (Win / Linux / macOS) + postojeći `qnc-host`, isti bin lokalno / LAN / internet, DB-first, **bez obaveznog uploada medija**, bez JS/HTML/WebView u native UI-ju.

---

## 2. Write target i legacy

| Lokacija | Uloga |
|----------|--------|
| `C:\Users\miron\Projects\QNC` | **Kanonski** native migracijski track |
| `quick_news_cutter` | Stariji / paralelni tree — ne miješati commitove bez dogovora |
| `app/`, `plugins/` u QNC | Legacy web — samo referenca toka/API dok traje migracija |
| `qnc-av-player-wasm`, `docs/qnc-elements.md`, custom HTML tagovi | **ODBAČENO** — ne produkt, ne lab track, ne fallback |

**WASM / web custom elements (`<qnc-av-player>`, …) ne koristimo.**  
Sporiji i teži put (browser/WebView + WASM decode/bridge), bez broadcast kontrole frame timinga; ne zamjenjuju native `qnc-app`. Postojeći folderi/docovi ostaju arhiva za brisanje kad zatreba — **ne razvijati dalje**.

Pravilo: native track ne forká logiku u dva repoa odjednom.

---

## 3. Usklađenje s `qnc-client-server.md`

| Client-server (kanonski) | Native plan | Odluka |
|--------------------------|-------------|--------|
| Jedan nepromijenjen klijent | `qnc-app` isti bin | **Zadržati** |
| Profili `workstation` / `online_test` | Local / LAN / Internet | Mapirati 1:1 (vidi §4) |
| Mediji ne putuju obavezno | `MediaAccessMode`, path mapping | **Zadržati**; internet ≠ default upload |
| SQLite + Rust API = istina | Host ostaje backend | **Zadržati** |
| P1 workstation prvo | Milestone 1 Project-first | **Zadržati** |
| P2 online_test + media agent | Faza Internet + agent | Tek nakon local/LAN MVP |
| P3 house_ref / Bridge | Kasnije | Ne u MVP |
| P4 multi-user live | Van scopea | Ne u ovom planu |
| `upload_required: false` | Internet smije *opcijski* upload | Upload samo eksplicitni mode, nikad tihi default |

### Mapiranje profila

| Profil (client-server) | `qnc-app` runtime | Baza | Mediji |
|------------------------|-------------------|------|--------|
| `workstation` | Local ili LAN URL | host SQLite | `ServerLocalPath` / `SharedFilesystem` / `ClientLocalPath` + mapping |
| `online_test` + teren | Internet HTTPS | remote host | **local media agent** (decode lokalno); editorial API → remote |
| `online_test` + kuća | Internet HTTPS | remote host | `server_seed` / `ProxyOnly` |
| (kasnije) house | Local + refs | host | `house_ingest_ref` read-only |

Capabilities odgovor hosta ostaje ugovor (proširiti `/api/shell/runtime` ili `GET /api/runtime/capabilities` kako piše native plan Faza 1).

---

## 4. Playback ownership — host vs client

Ovo je bila glavna napetost. **Zatvorena odluka:**

### 4.1 Istina vremena i editorial plana — uvijek host

| Odgovornost | Vlasnik | Napomena |
|-------------|---------|----------|
| Playlist / parts / covers / markers | **Host + SQLite** | Klijent šalje intent, ne IN/OUT kao istinu |
| `TimelineModel` snapshot | **Host** | Native samo crta |
| Export / render plan | **Host** | Serializiran, reproducibilan; ne UI state |
| Session clock za **wrap / mixed story preview** | **Host `PlaybackSession`** | Active layer part/cover; mixed A1(+A2) |

### 4.2 Decode surface — po načinu rada

| Scenario | Decode / frame izvor | Zašto |
|----------|----------------------|--------|
| **Wrap / story preview** (A1+V+A2, cover mix) | **Host** proxy-first → frame JPEG + mixed audio | Jedini način za deterministički mix slojeva; isti kao F2 |
| **Source / All scrub** kad client vidi file (`ClientLocalPath` / shared map) | **Client-local decode** (FFmpeg u `qnc-app`) preferiran | Niža latencija, manje mreže; fingerprint mora matchati host meta |
| **Source** kad client **ne** vidi file | **Host** proxy frame/audio API | Nema tihog pretpostavljanja patha |
| **Original** (MXF/XAVC…) | Reserved — export / open-source kasnije | Ne default scrub path; `proxy_missing` ≠ fallback na original bez eksplicitnog moda |
| ~~Web / WASM~~ | — | **Odbaceno** — nema WASM player puta |

```text
                    ┌─ Wrap / mix preview ──► Host PlaybackSession (proxy)
PlaybackRequest ───┤
                    └─ Source scrub ────────► Client decode AKO MediaAccess to dozvoli
                                              inače Host proxy frames
```

### 4.3 Timebase

- Editorial i timeline UI: **frame-based** (`FrameNumber` + fps / drop-frame), ne float-second kao istina.
- API smije i dalje primati/slati sekunde kao transport, ali native + host normaliziraju u frameove.
- Playhead u UI i u `PlaybackSession` moraju dijeliti isti timebase ugovor (dokumentirati u API contract Fazi 1).

### 4.4 Što klijent nikad ne radi

- Ne mergea template settings.
- Ne drži editorial playlist kao istinu.
- Ne pretpostavlja da je server path valjan na clientu.
- Ne tiho uploadа medije jer je „lakše”.
- Ne koristi HTML `<video>` / WebView kao montažni motor.

---

## 5. Ciljna arhitektura (sažetak)

```text
qnc-app (native: winit + wgpu + egui)
  ├─ AppState machine (Booting → ProjectOnly → WorkspaceLoaded)
  ├─ QncModule: Project / Ingest / Story / …
  ├─ HostClient (reqwest) → qnc-host API
  ├─ MediaAccess + path mapping
  └─ playback/
       ├─ host_session  (wrap / fallback source)
       └─ local_decoder (source kad file dostupan)

qnc-host
  ├─ SQLite istina
  ├─ ingest / proxy workers
  ├─ TimelineModel + story API
  └─ PlaybackSession (proxy-first mix)

qnc-media-agent   ← samo online_test teren (P2), ne MVP
```

Stack iz native plana ostaje: **winit + wgpu + egui + ffmpeg + cpal/rodio + tokio + reqwest**.  
**Ne:** Tauri, WebView, Python, 1:1 copy web layouta.

---

## 6. Faze implementacije (kad krene kod)

Usklađeno s native planom Faza 1–11; rebrojano s playback odlukom.

| # | Faza | Done kada |
|---|------|-----------|
| **M0** | Ugovori | Ovaj doc + capabilities skica + API lista prihvaćeni; write target = QNC |
| **M1** | API contract | `health`, `runtime/capabilities`, project open/workspace, ingest/story read surface stabilni |
| **M2** | `qnc-app` skeleton | Window, connect, HostDisconnected, Project-only |
| **M3** | Project screen | Lista / open / create / workspace load → entry screen |
| **M4** | Workflow shell | Screen router iz workspacea; bez projekta → Project only |
| **M5** | Ingest | Folder pick, discover, grid, import progress (isti host workeri) |
| **M6** | Media access | `MediaAccessMode` + path mapping + availability status |
| **M7a** | Playback host path | Wrap/story preview: Host session → frame + mixed audio u native surface |
| **M7b** | Playback local source | Source scrub: client decode kad access dozvoli; inače host fallback |
| **M8** | Story UI | Timeline (wgpu/egui paint), parts/markers/covers intents → API |
| **M9** | Broadcast kriteriji | Timecode, fps set, sync, export plan serializacija (iz native Faza 9) |
| **M10** | Packaging | Win first; zatim Linux/macOS smoke |
| **M11** | LAN harden | Mapping + reconnect; bez pretpostavke istih pathova |
| **M12** | online_test | Auth + agent ili seed; `upload_required: false` default |
| **M13** | Web sunset | Tek nakon parity: legacy web UI ugasiti; WASM/elements obrisati ili archived, bez daljnjeg razvoja |

### Milestone 1 (prvi dokaz — bez Story/playback)

```text
qnc-app
  → connect qnc-host
  → Project-only
  → open project
  → load workspace
  → Ingest placeholder
```

To je **ulaznica** za M5+; ne čekati puni Story.

---

## 7. API površina (MVP ugovor)

Minimalni set (iz native Faza 1) — ne širiti dok M1 nije zelen:

```text
GET  /api/health
GET  /api/runtime/capabilities   # ili formalizirati /api/shell/runtime
GET  /api/projects
POST /api/projects/open
POST /api/projects/from-template
GET  /api/projects/{id}/workspace

GET  /api/ingest/state
POST /api/ingest/browse | discover | import

GET  /api/story/state
GET  /api/story/timeline-model
POST /api/story/playback/start | stop | seek | pause
GET  /api/story/playback/state | frame | audio

GET  /api/media/resolve | probe | frame | waveform
```

Playback rute moraju biti na **živom** Story (ili neutralnom `/api/playback`) mountu — ne mrtvi orphan qstory-only path.

---

## 8. Definition of Done — native MVP

1. `qnc-app` radi bez JS/HTML/WebView.  
2. Project-first tok identičan pravilima (open → workspace → entry).  
3. Ingest: discover/import preko host API.  
4. Story: timeline iz `TimelineModel` + wrap preview preko host playback.  
5. Source scrub koristi local decode samo uz validan `MediaAccessMode`.  
6. Local + LAN: nema obaveznog uploada.  
7. Timebase frame-based; export plan ne ovisi o UI-only stateu.  
8. Win binary smoke; Linux ili macOS barem build/health.

**Van MVP:** Bridge/house_ref, multi-user live, design-tools, full LUFS UI, 3–4 audio kanala.  
**Trajno van produkta:** WASM, WebView/Tauri, HTML custom elements kao player/timeline.

---

## 9. Što ne raditi

- Ne prepisivati Story u prvom milestonu.  
- Ne uvoditi Tauri/WebView kao „brzi native”.  
- Ne razvijati WASM niti web custom elements (`<qnc-av-player>`, `<qnc-timeline>`, …) — odbaceno.  
- Ne hardcodirati `C:\` / `/home/…` u shared kodu.  
- Ne pretpostaviti da LAN client i host vide isti path.  
- Ne big-bang gasiti web prije M13.  
- Ne commitati runtime DB / proxy bench binarije.

---

## 10. Odluke zatvorene ovim docom

| ID | Odluka |
|----|--------|
| D1 | UI: winit + wgpu + egui |
| D2 | Host zaseban proces |
| D3 | Editorial istina samo na hostu |
| D4 | Wrap/mix playback = host proxy-first; source scrub = client decode ako access dozvoli |
| D5 | online_test / auth / agent = nakon local+LAN MVP |
| D6 | Legacy web parallel samo dok native ne pokrije parity; zatim sunset |
| D8 | WASM + web custom elements = **odbaceno** (ne koristiti) |
| D7 | Write target native tracka = `Projects\QNC` |

---

## 11. Aktivacija / stanje

| Milestone | Stanje |
|-----------|--------|
| M1 Project-first shell | **Implementiran** — connect, ProjectOnly, open, workspace |
| M3 Project screen | **Implementiran** — list/create/templates/paths/delete (host ui-state) |
| M5 Ingest native | **Implementiran** — toolbar, poster cards, Otkrij/Uvezi, archive, poll |
| M7a + M8 (partial) Story | **Implementiran** — TimelineModel + host frame/audio + wrap paint |
| Story edit | **Implementiran** — All/Virtual/Segment, parts, Mark IN/OUT, covers, markers, commit/Export UI |
| WebView / wry omotač | **ODBAČEN** — produkt je egui `qnc-app`, ne HTML |
| M6 media access / M7b local decode | Čeka „kreni …” |

```powershell
cargo build -p qnc-app
cargo run -p qnc-app -- --host http://127.0.0.1:8001
```
