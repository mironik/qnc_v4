# QNC DB-first architecture contract

**Status:** ratificirano (Phase 0)  
**Obvezno za:** sve izmjene u `QNC_v2/quick_news_cutter`  
**Supersedes:** implicitne “ephemeral/local state” iznimke u plugin JS-u

---

## 1. Core rule

**QNC is database-first from beginning to end.**

| Layer | Source of truth |
|-------|-----------------|
| Application / workflow / project / plugin state | **SQLite** via **Rust API** |
| UI | **Projections** of API snapshots — never the owner |
| `ctx.store` | **Short-lived cache** of GET snapshots — not truth |
| JSON on disk | **Declarative manifests and static config only** |

If a value affects workflow and is not in SQLite, **it is not application state**.

---

## 2. Allowed vs forbidden

### 2.1 SQLite / Rust API

| Allowed | Forbidden |
|---------|-----------|
| Read/write workflow state only through Axum routes | Business logic in plugin JS that mutates workflow without API |
| One global DB (`data/project_store.db`) + one per-project DB (`qnc_project.db`) | Separate plugin-local JSON/JS stores as truth |
| Snapshots returned as JSON from GET routes | Helper JSON files used at runtime for workflow |

### 2.2 Plugin orchestrator JS

| Allowed | Forbidden |
|---------|-----------|
| `QNC.createPluginApp`, lifecycle hooks | Workflow fields in `pool`, `state`, or ad-hoc globals |
| `ctx.on` → `ctx.action` → `ctx.store.reload` → render | Skipping reload after write |
| Map snapshot → `component.update(model)` | `fetch` / `QNC.api` for workflow reads that bypass declared snapshots (except one-off shell/system calls) |
| Shell bus for **invalidation** (`project:changed`) | Direct calls/imports between plugins; exposing mutable globals (`QNC.mediaPool = pool`) |

**media_pool orchestrator — technical handles vs workflow (strict):**

| Allowed locally (not truth) | Forbidden locally (must be SQLite / API snapshot) |
|------------------------------|-----------------------------------------------------|
| Timer / poll handle ids | Transcript text, transcript status, ASR job state |
| `AbortController` for canceling in-flight streams | Per-row status, selected clip ids, current clip id |
| DOM / `<video>` element refs, listener cleanup hooks | Mark in/out, active virtual shot, filmstrip build status |
| Live SSE dedup tokens, thumb cache-bust rev (render-only) | Any parallel JS object mirroring snapshot workflow fields |

If transcript data or status exists, it must be read/written via Rust API → `qnc_project.db` (e.g. `clip_transcripts`), then reflected through `ctx.store.reload` — never retained as orchestrator workflow state.

### 2.3 Component JS

| Allowed | Forbidden |
|---------|-----------|
| `mount` / `update(model)` from orchestrator | Owning business/workflow state |
| Emit user intent via `QNC.componentBus` | Calling plugin APIs, SQLite, or other tabs |
| Technical mount flags (`dataset.qncComponentMounted`) | Treating DOM as truth after navigation |

### 2.4 DOM

| Allowed | Forbidden |
|---------|-----------|
| Render model fields | Read checkbox/input/class as authoritative state after tab switch or reload |
| Emit click/change as **intent** | Persist workflow in attributes without DB round-trip |

### 2.5 JSON files

| Allowed (A — declarative) | Forbidden (B — runtime state) |
|---------------------------|-------------------------------|
| `plugins/*/plugin.json` | ~~`data/shell_module_state.json`~~ (migrated Phase 1 → `project_store.db`) |
| `app/components/registry.json`, `component.json` | `data/projects.json` as live mirror (export/migration only) |
| `plugins/project/storage/system_seed.json` | ~~`data/design_overrides/*.json`~~ (migrated → `app_settings` `design.*`) |
| `app/shell/keyboard-shortcuts.json` (defaults) | Any host-written JSON holding workflow state long-term |
| Design-tools demo/build-profile JSON under `plugins/design-tools/` | Using demo JSON in production workflow paths |

**Rule:** JSON may describe *what exists* (manifest). JSON must not *be* the running workflow.

### 2.6 Project filesystem vs project database

The project directory may contain media files and regenerable render artefacts, but it must not contain durable workflow truth outside SQLite.

| Allowed in project filesystem | Must be in `{project_dir}/qnc_project.db` |
|-------------------------------|-------------------------------------------|
| Original media copies when project policy requires archive | Virtual shots, source/derived frame ranges, source FPS/timebase |
| Proxy media generated from source media | Story parts, covers, marker/effect rows, audio bus decisions |
| Filmstrip JPGs and preview frame cache, because they are derived artefacts | Filmstrip status/index/seek metadata used by UI/API |
| Temporary/export work products | Transcripts, ASR status, selected clip/shot state, IN/OUT |

If deleting a non-media cache directory changes the edit decision, the design is wrong: that data belongs in SQLite.

---

## 3. Mandatory orchestrator flow

Every plugin tab must follow this loop for workflow data:

```
component event (user intent)
  → ctx.on handler
  → ctx.action(actionId, body)     // write path
  → Rust API
  → SQLite write
  → ctx.store.reload(snapshotKey)  // read path
  → render from snapshot (component.update)
```

Read-only tab show:

```
onShow
  → ctx.store.reload(snapshotKey)
  → render from snapshot
```

**No step may be skipped** for workflow-affecting operations.

---

## 4. Database ownership (current)

| Database | Scope | Owns |
|----------|-------|------|
| `data/project_store.db` | Global | Projects, active project, templates, collab users/sessions, project tab UI state, **design editor** state (`app_settings` `design.*`) |
| `{project_dir}/qnc_project.db` | Per project | Project settings, workflow steps, **ingest** assets/meta, **media_pool** pool_clips, virtual_shots (typed columns), workflow + selection tables, **clip_transcripts** + segments, **filmstrip** tables |

Modules enable flags live in **`project_store.db` → `module_state`** (Phase 1). Legacy `data/shell_module_state.json` is imported once on host start and renamed to `.migrated`.

---

## 4.1 Story editorial truth

Story montage is based on durable virtual entities, not transient UI trims.

There are two virtual-shot types:

- **Source virtual shot** (`kind = 'import_root'`) — the whole source clip; shown in Story **All** tab.
- **Virtual shot** (`kind = 'virtual'`) — a frame range cut from one source virtual shot; shown in Story **Virtual** tab.

Story also has **virtual segments**:

- **Virtual segment** (`story_parts.part_id`) — TON/OFF/Story segment shown in Story **Segment** tab.
- A virtual segment may reference a `virtual_shot_id`, or it may store its own `clip_id + in_frame/out_frame` source range directly in `story_parts`.
- Creating a Segment-tab virtual segment must not implicitly create a derived Virtual-tab shot.

| Field | Durable source of truth |
|-------|-------------------------|
| Source clip identity | `virtual_shots.clip_id` / `ingest_assets.clip_id` |
| Source IN/OUT | `virtual_shots.in_frame` / `virtual_shots.out_frame` |
| Source FPS/timebase | source file probe stored in `ingest_assets.fps`, mirrored to virtual-shot source metadata |
| Story virtual segments | `story_parts.part_id` with `clip_id`, `virtual_shot_id` when applicable, `in_frame`, `out_frame`, `fps` |
| Story covers | references to `virtual_shot_id`; host resolves clip/frame range from SQLite |
| UI playhead/scrub | runtime-only cursor; never authoritative editorial state |

Rules:

1. A saved Virtual-tab edit must become a derived `virtual_shot_id`.
2. A saved Segment-tab edit must become a durable virtual segment in `story_parts`; it does not have to create or reference a derived `virtual_shot_id`.
3. Playback/export must resolve source clip, frame range and FPS from Rust API + SQLite, not from client-only state.
4. FPS is source-file metadata from probe/DB. Project/export FPS is only an export setting and must not set source, virtual-shot, Segment/Story timeline, marker, slot, or playback math.
5. News export may be p50 or i50; `fps=25 + upper_first + i50` is interlaced 50-field delivery. PAL single-rate progressive export is not a valid project/export profile.
6. If client and host disagree, host database wins; client state is discarded/reloaded.
7. Story/program playback uses a streaming playlist input: open only the current item and a small forward window, never decode the whole playlist at open/play start, and never silently drop video frames or audio packets.

---

## 5. ctx.store contract

`ctx.store` is implemented in `app/shell/qnc-plugin-sdk.js`.

| API | Role |
|-----|------|
| `load` / `reload` | Fetch snapshot from Rust GET route; update in-memory cache |
| `get` | Read cache for **render only** — after load/reload |
| `invalidate` | Mark stale; refresh on next `onShow` |
| `subscribe` | Optional; most tabs re-render after reload |

**Requirements:**

1. After every **write** (`ctx.action`), call `ctx.store.reload` for affected snapshot keys.
2. Render functions read workflow fields from `ctx.store.get(...)`, not from parallel JS objects.
3. Cache may be discarded on tab hide/destroy without losing workflow (SQLite retains truth).

---

## 6. Known gaps (audit snapshot — do not treat as policy)

These violate this contract until migrated (see roadmap in audit / Phase 1–5):

| Area | Current | Target |
|------|---------|--------|
| Module enable | ~~`data/shell_module_state.json`~~ → **`project_store.db` `module_state`** (Phase 1 ✓) |
| media_pool | ~~JS caches~~ → **Phase 5 ✓** workflow/selection/transcripts/virtual_shots in typed SQLite tables |
| project tab | ~~Large `state` object cache~~ → **Phase 2 ✓** SDK snapshots (`project.index`, `project.templates`, `project.modules`, `project.ui`) |
| design-tools | ~~`data/design_overrides/*.json`~~ → **`app_settings` `design.*`** in `project_store.db` (non-production add-on) |
| sdk_demo | ~~In-memory Rust map~~ → **Phase 4 ✓** `sdk_demo_state` in `qnc_project.db` (demo template only) |
| Shell | ~~`QNC.activeProjectId` in JS~~ → **Phase 4 ✓** boot sync from `GET /api/projects`; projection only |
| Keyboard shortcuts | ~~`localStorage`~~ → **Phase 4 ✓** `app_settings.keyboard_shortcuts_user` |

New features **must not** add rows to this gap list.

---

## 7. Cross-plugin communication

| Allowed | Forbidden |
|---------|-----------|
| Read shared data via **SQLite** (each plugin’s own API routes) | Import another plugin’s JS |
| Shell bus: `project:changed`, `project:deleting`, `project:opened` as **signals** | `QNC.mediaPool`, shared mutable globals |
| Shared `project_id` from shell context | Plugin A calling Plugin B HTTP namespace directly from orchestrator |

---

## 8. Reference plugins (strict reading)

| Plugin | DB-first status |
|--------|-----------------|
| **ingest** | **Reference** — workflow in SQLite; SDK snapshot + reload |
| **sdk_demo** | Minimal SDK template — counter in `qnc_project.db` (`sdk_demo_state`); tab disabled by default |
| **media_pool** | **SDK v1** — clips, workflow, virtual shots, transcripts in SQLite. Orchestrator JS: **technical handles only** (timers, `AbortController`, DOM/media refs, listener cleanup, in-flight dedup). **No local workflow:** transcript/ASR/row status, selection, marks, active shot, filmstrip status — DB/API snapshots only. |
| **project** | **SDK v1** — index/templates/modules/ui via `ctx.store`; ephemeral runtime only (`openingId`, collab session handle) |
| **design-tools** | **Non-production add-on** — theme/lab prefs in `app_settings` (`design.*`); not core workflow |
| **qstory** | **SDK v1** — montaža (segmenti, markeri, coveri, odabiri) u SQLite; snapshot preko `qstory-state-store`. **Runtime only (nije u bazi):** `sourceDraft`, playhead, source editor kontekst — vidi [qstory-persistence.md](qstory-persistence.md) |

---

## 9. Compliance checklist (for PRs)

- [ ] Workflow read comes from API snapshot (via `ctx.store` or equivalent reload-after-fetch).
- [ ] Workflow write goes through Rust API → SQLite.
- [ ] `ctx.store.reload` after every write action.
- [ ] No new runtime JSON state files.
- [ ] No new plugin-to-plugin JS coupling.
- [ ] Components receive model; emit events only.
- [ ] No DOM-as-state for workflow fields.
- [ ] DB schema changes documented and approved separately.

---

## 10. Related documents

- [development-policy.md](development-policy.md) — laptop-first, Rust host
- [shell-spec-v1.md](shell-spec-v1.md) — shell API, storage boundaries
- [plugin-sdk-v1.md](plugin-sdk-v1.md) — orchestrator golden path
- [developer-components.md](developer-components.md) — component contracts
- [create-plugin-from-sdk-demo.md](create-plugin-from-sdk-demo.md) — minimal plugin scaffold

---

*Izmjene ovog ugovora samo kroz reviziju ovog dokumenta i sinkronizaciju povezanih specifikacija.*
