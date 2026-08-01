# QNC audit — trenutno stanje (2026-07-17)

**Tip:** read-only snimka radnog treea.  
**Freeze:** samo Project forma (`qnc-hard-freeze-hardware-only`).  
**Povezano:** [qnc-step-plan.md](qnc-step-plan.md), [qnc-client-server.md](qnc-client-server.md).

---

## Verdict

Repo je u **mid-cleanup** stanju: default workflow je **Project → Ingest → Story** (`storyboard`), ali **QStory je half-deleted** — UI plugin obrisan s diska, dok `registry.json`, `db-first-guard` i dijelovi `test.ps1` još očekuju QStory. Zbog toga **`.\test.ps1` pada na prvom koraku**. Hard freeze dozvoljava samo Project rad.

---

## Plugin mapa

| plugin_id | enabled | tab_id | Stanje |
|-----------|---------|--------|--------|
| `project` | true | `project` | allow (freeze) |
| `ingest` | true | `ingest` | freeze |
| `story` | true | `storyboard` | freeze |
| `media_pool` | false | `pool` | API nije mountan |
| `design-tools` | true | `design-tools` | non_production |
| `sdk_demo` | false | `sdk_demo` | demo |
| `qstory` | — | — | **nema na disku** (git `D`) |

`system_seed.json` — svi templatei: `workspace.tabs = ["project","ingest","storyboard"]`.

---

## Host API

**Mounted:** project, ingest, story, design, asr, sdk_demo.  
**Off:** media_pool, qstory (modul + PlaybackStore još u kodu; vidi `qnc-host/src/qstory/ABANDONED.md`).

---

## Komponente

- Živo: `project-*`, `ingest-*` (+ `ingest-dir-tree`), `story-*`, `kodak-timeline`, `media-pool-*`, `filmstrip-viewer`, …
- Obrisano s diska: `qstory-*`, `editorial-timeline`, `timeline-sequence`, `story-cut-part`
- **Stale:** `registry.json` još drži ~12 `qstory-*` unosa na obrisane putanje

---

## qnc-client

Workspace member (`Cargo.toml`: `qnc-host`, `qnc-client`). CLI: `health`, `play`, `play-clip`; egui `--gui`.

| Put | API | Stanje |
|-----|-----|--------|
| editorial `play` | `/api/qstory/playback/*` | mrtav (router off) |
| `play-clip` | `/api/story` | živ put |

---

## Test / guard

| Alat | Problem |
|------|---------|
| `scripts/db-first-guard.ps1` | Traži `plugins/qstory/static/qnc-qstory.js` (+ playback, store, …) → **Fail** |
| `test.ps1` | Pokreće guard prvi → **crven** |

---

## Top 5 rizika

1. **QStory half-delete** — disk vs registry/guard/test → CI/lokalni test crven.
2. **`data/project_store.db`** tracked + modified (ignore ne pomaže za već tracked fajl).
3. **`data/_proxy_bench/`** ~416 MB untracked medija.
4. **Dual editorial stack** — Story web + qstory Rust leftover + client na qstory URL-ovima.
5. **Doc drift** — stari ABANDONED/audit vs kodak još na disku.

---

## Freeze — što smije

| Rad | Dozvoljeno? |
|-----|-------------|
| Project UI/API/seed | **Da** |
| Ingest | Ne — dok „odmrznuti ingest” |
| Story / kodak / wrap | Ne — dok „odmrznuti story” |
| qnc-client | Ne — nije u allow-listi |
| Commit/push | Ne — bez eksplicitnog zahtjeva |

---

## Preporučeni sljedeći korak (unutar freeze)

1. Stabilizirati **Project formu** (template/settings, open/create/delete).
2. Ne stagingati `project_store.db` ni `_proxy_bench`.
3. Čišćenje qstory ghostova / guard / test — **tek nakon** eksplicitnog odmrzavanja ili posebnog dogovora.
