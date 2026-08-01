# QNC — plan razvoja (step by step)

**Status:** dogovoreni smjer (2026-07)  
**Cilj:** stabilan QNC-timeline, native klijent + Rust server, virtual-source, bez web playback kompromisa.

**Povezano:**  
- [qnc-client-server.md](qnc-client-server.md) — **kanonski deployment plan** (workstation / online_test, no-upload)  
- [qnc-elements.md](qnc-elements.md) — `<qnc-av-player>`, `<qnc-timeline>`, paneli  
- [architecture-db-first.md](architecture-db-first.md), [qstory-editorial-playlist.md](abandoned/qstory-editorial-playlist.md) (ABANDONED)

---

## Načela (ne kršiti)

1. **SQLite + Rust API** = jedina istina; UI je projekcija.
2. **Virtual-source** kroz cijeli lanac (import_root → virtual_shot → part/cover → stream IN/OUT).
3. **Story UI** = isti Shell izgled i proces rada. Custom elementi:
   `<qnc-timeline>`, `<qnc-av-player>`, `<qnc-wrap-panel>`, `<qnc-all-virtual-segment-panel>`.
   Hostanje: **običan browser** (univerzalno testiranje) → kasnije opcionalno `qnc-client` + WebView
   koji učitava samo naš Shell (ne vlastiti HTML engine).
4. **Playback u Rustu** — nema `<video>` / Web Audio kao A/V motor; preview ide kroz `<qnc-av-player>` + host API.
5. **TV kuća:** ingest spine netaknut; Bridge kasnije read-only; produkcija = **workstation po stanici** (ne obavezni centralni QNC SPOF).
6. **Teren / stanica:** projekt + mediji uz lokalni `qnc-host`.
7. **Nema obaveznog uploada medija** — vidi [qnc-client-server.md](qnc-client-server.md).
8. QStory nije dio projekta. Gol egui transport **nije** produktni Story UI.

---

## Arhitektura (cilj)

```
[QNC Client — isti]  ←HTTP→  [qnc-host — Rust]
                                  ├─ SQLite
                                  ├─ PlaybackSession (proxy-first)
                                  ├─ MediaResolver (local_file | house_ingest_ref)
                                  └─ Export

Produkcija:  host živi na radnoj stanici (teren = laptop; TV = svaki montažni PC)
Online test: baza na našem serveru; mediji lokalni (agent) ili seed — bez uploada
TV ingest:   [Ingest kuće] → read-only → [QNC Bridge] → registracija refova
```

**Deployment:** `workstation` | `online_test` | (kasnije namjerno dijeljenje) — detalji u [qnc-client-server.md](qnc-client-server.md).

---

## Logički redoslijed (glavni)

```text
Ugovori (F1 / P0)
  → Workstation montaža (F2–F5 / P1)     ← kritični put
  → Online test no-upload (P2)           ← parkiran do prioriteta
  → MediaResolver + Bridge (F3 / F7 / P3)
  → Handoff teren→kuća (F8)
  → Namjerno dijeljenje (P4)             ← opcionalno
```

---

## Faza 0 — Zamrzavanje web playbacka

| Korak | Akcija | Done |
|-------|--------|------|
| 0.1 | QStory web playback frozen (nema novih featurea) | — |
| 0.2 | Bugfixevi samo ako blokiraju ingest/project | — |
| 0.3 | Editorial model referenca | ✓ doc |

---

## Faza 1 — Ugovori

| Korak | Deliverable | Status |
|-------|-------------|--------|
| 1.1 | `docs/qnc-timeline.md` | ✓ |
| 1.2 | `docs/qnc-client-server.md` | ✓ dogovoreno, implementacija parkiranа |
| 1.3 | `docs/qnc-virtual-source.md` | pending (prvi korak kad se vrati deployment track) |
| 1.4 | web kodak deleted; native qnc-timeline | ✓ |

---

## Faza 2 — Rust playback (MVP)

| Korak | Status |
|-------|--------|
| 2.1–2.5 session, mix, audio, frame | ✓ |
| 2.6 regresijski curl / smoke | po potrebi |

---

## Faza 3 — MediaResolver + virtual-source

| Korak | Akcija |
|-------|--------|
| 3.1 | `resolve_proxy_path` / `resolve_original_path` |
| 3.2 | `LocalProjectDir` (workstation) |
| 3.3 | `HouseIngestRef` |
| 3.4 | `ingest_assets.source_kind`: `local_file` \| `house_ingest_ref` |
| 3.5 | playback / virtual-stream koristi resolver |

---

## Faza 4 — Native klijent (transport)

| Korak | Status |
|-------|--------|
| 4.1–4.6 egui, play/seek, rodio, frame, local | ✓ MVP |

---

## Faza 5 — Story u native clientu

**Story** = UI + timeline u **`qnc-client`**; host = `/api/story` + `timeline_model`.  
Primjene: source = IN/OUT; wrap = + M + covers.  
**QStory nije dio projekta.** Web kodak obrisan.  
Testeri / mreža: Faza 9 + [qnc-client-server.md](qnc-client-server.md) (`online_test`).  
Detalj: [qnc-timeline.md](qnc-timeline.md).

| Korak | Status |
|-------|--------|
| 5.1 TimelineModel API (Story) | ✓ |
| 5.2 Paint A1/V/A2 (`qnc-client`) | ✓ |
| 5.3 I/O + T/V intent → DB | ✓ |
| 5.4 Wrap stack (native Story) | ✓ |
| 5.5 Shell Story UI + `<qnc-av-player>` / `<qnc-timeline>` | scaffold ✓ — nema HTML video; isti layout |
| 5.6 Source vs wrap primjene | ✓ |

---

## Faza 6 — (zastarjelo kao default)

~~Centralni LAN server kao default za TV kuću~~ — **poništeno kao default**.  

Produkcija TV = **P3 workstation po stanici** ([qnc-client-server.md](qnc-client-server.md)).  
Centralni dijeljeni host = samo **P4** (namjerno).

---

## Faza 7 — QNC Bridge (TV ingest, read-only)

| Korak | Akcija |
|-------|--------|
| 7.1 | `qnc-bridge` zaseban bin |
| 7.2 | watch_folder na kućni `done/` |
| 7.3 | register-virtual-source (read-only) |
| 7.4 | house proxy ili sidecar cache |
| 7.5 | pilot jedna emisija; rollback = ugasi Bridge |

---

## Faza 8 — Handoff teren → kuća

| Korak | Akcija |
|-------|--------|
| 8.1 | Export: `qnc_project.db` + proxy + manifest |
| 8.2 | Import + re-link na `house_ingest_ref` |
| 8.3 | Virtual IN/OUT ostaje |

---

## Faza 9 — Online test / remote (iz [qnc-client-server.md](qnc-client-server.md) P2)

**Parkirano.** Kad krene: remote DB + local media agent + seed kuća-mod; `upload_required=false`.

(Stari „thin client sve na kući“ više nije fokus; no-upload + lokalni mediji jesu.)

---

## MVP (prvi „gotovo“)

1. Workstation: native/local host, jedan projekt, wrap play, A1 OFF / A1+A2 cover.  
2. Virtual-source u bazi; fajlovi u projekt diru.  
3. Web montaža nije potrebna za demo.

---

## Trenutni fokus

| Track | Stanje |
|-------|--------|
| Deployment / online_test / media agent | **Plan gotov, kod parkiran** — vidi [qnc-client-server.md](qnc-client-server.md) §10 |
| Aktivni rad | **Drugi prioritet** (po dogovoru) |

Kad se vrati deployment track: **1.3 `qnc-virtual-source.md`**, zatim P1/P2 iz client-server plana.

