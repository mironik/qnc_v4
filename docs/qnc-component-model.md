# QNC Component Model

Status: active native contract for `qnc_v4`.

This document defines the Lego component model used by `qnc-app`. It replaces
old web/plugin assumptions for native UI work. The product UI is Rust/egui only.

## Core Rule

Forms are layout hosts. They are not owners of workflow, media, playback,
timeline, shortcut, marker, cover, import, or project truth.

Durable truth lives in SQLite through `qnc-host` Rust API. Runtime playback truth
lives in the broadcast player. UI components only project snapshots and emit
neutral intents.

## Anchors

| Anchor | Ownership |
| --- | --- |
| SQLite / Rust API | Durable project/workflow/source/editorial state |
| Broadcast player | Runtime playback clock, source frame, transport state |
| Component runtime | Async command execution and stale-result filtering |
| Media asset loaders | Async UI-only thumbnails, poster images, filmstrip, waveform assets |
| Forms | Passive layout and local display/draft state |
| `qnc_timeline` | Passive painter of supplied frame/layer/range model |

Seconds and timecode strings are display values only. Frame numbers and source
FPS are active math.

## Component Envelope

Every backend-facing component command uses the same neutral envelope:

| Field | Meaning |
| --- | --- |
| `component_id` | Stable component family, e.g. `editorial.edit` |
| `port_id` | Independent channel inside the component |
| `operation_id` | Specific operation on the port |
| `request_key` | Correlation scope for stale-result filtering |
| `method` / `path` / `payload` | HTTP request description |
| `timeout` | Host request timeout class |
| `result_policy` | Currently `LatestWins` |

The component runtime does not know Story, Media Assist, Ingest, Project, or any
future form. It executes envelopes and returns `ComponentBackendEvent`.

## Runtime Behavior

`ComponentBackendRuntime` uses a fixed worker pool, not thread-per-submit.
Current default: 4 workers.

`LatestWins` is scoped by:

```text
component_id + port_id + request_key
```

An older queued request for the same correlation key is skipped before execution
when a newer request has already replaced it. If it becomes stale while an HTTP
request is already running, the response is not emitted back to the UI. This
does not abort an already running `ureq` call; it prevents stale UI projection
and reduces avoidable backend work during bursts.

Ports must be split when operations must not suppress each other. Example:
project catalog uses separate ports for templates, modules, keyboard presets,
default root, and UI state.

## Request Keys

Request keys are component-local, but they must be stable and explicit.

Allowed patterns:

| Use case | Example |
| --- | --- |
| Project-scoped latest state | `project_id` |
| Reusable screen instance | `instance_id + project_id` |
| User edit mutation | `instance_id + project_id + request_id + detail` |
| Global static catalog | `global` |

Do not encode workflow meaning in a form name. `instance_id` only separates two
mounted instances of the same neutral component, for example `story` and
`media_assist`.

## Current Native Components

| Component | Role |
| --- | --- |
| `shell.state` | Shell health/runtime/appearance/workspace snapshots |
| `project.registry` | Project list snapshot |
| `project.catalog` | Project templates/modules/keyboard/default-root/UI-state snapshots |
| `project.command` | Project open/create/delete/settings/template mutations |
| `filesystem.list` | Local filesystem listing |
| `theme.picker` | Appearance save command |
| `source.import.state` | Ingest state load/poll |
| `source.import.selection` | Ingest clip selection mutation |
| `source.import.command` | Ingest browse/discover/select/import mutations |
| `editorial.state` | Story/editorial state + timeline model snapshots |
| `editorial.edit` | Editorial DB mutations: virtual shot, segment, marker, cover, commit |
| `shortcut.bindings` | Keyboard catalog + user binding snapshots for any mounted UI host |
| `media_assets` | Neutral async image/source-media asset loader used by mounted UI hosts |

## Editorial Flow

Initial load:

```text
layout host enters screen
  -> editorial.state story.state + timeline.model
  -> shortcut.bindings catalog + user
  -> form projects snapshot into media pool, source dock, segment panel, timeline
```

Edit:

```text
button / shortcut / component intent
  -> editorial.edit command
  -> host writes DB and returns story state
  -> form applies returned story state
  -> app requests editorial.state timeline.model refresh
  -> wrap/source UI re-projects from snapshot
```

Source selection and segment selection use already loaded snapshot paths and
frame ranges. They must not call `story_play_media` or `story_part_select` from
the UI thread.

Ingest uses the same `shortcut.bindings` component with its own `instance_id`.
Shortcut handling must read the already received snapshot; it must not fetch the
keyboard catalog from the UI event path.

## Playback Boundary

Playback is not a component-runtime job.

UI emits `PlaybackTransportIntent`; app routing sends it to `PlaybackStack` and
the broadcast player. The broadcast player is the only runtime clock. Timeline,
monitor, source dock, and segment panel are projections of that clock.

Do not mix component backend commands with player transport commands.

## Forbidden Coupling

Do not:

- import one form from another form;
- let a form call another form;
- store workflow truth in form local state;
- add form-specific transport branches to broadcast player;
- add story semantics to `qnc_timeline`;
- use seconds as edit math;
- call blocking host reads from `handle_shortcuts`, tab entry, click selection,
  scrub, or paint paths;
- create a component named after a form when the behavior is reusable.

## Error Handling

Current state:

- `ComponentErrorBoundary` records all component submit/event errors by neutral
  envelope key.
- A successful event for the same component key clears that active error.
- Forms still project narrow errors locally when they are the best display
  owner.
- Shell/footer can show the latest active component error as a global fallback.

Rule for new component work: return errors through the component event path and
route them to the narrowest display owner. Do not hide backend errors in silent
logs.

## Known Remaining Work

| Item | Reason |
| --- | --- |
| In-flight cancellation | Runtime skips queued stale requests and drops stale responses, but cannot abort an already running synchronous `ureq` call |
| Remaining direct `HostClient` usage | Asset loaders still use generic `request_json` / URL building; no form-specific `HostClient` asset wrappers remain |
| Dead-code cleanup | Several frozen/player/timeline/UI helpers still warn as unused |

## Validation Checklist

Manual P0 smoke:

- Story open -> shot select -> timeline click -> Space.
- Mark IN/OUT -> Save virtual -> Virtual tab and DB frame bounds.
- TON/OFF -> Segment tab -> wrap playback.
- Story <-> Media Assist fast switching.
- Ingest browse -> discover -> select -> import.

Automated minimum after component changes:

```powershell
cargo check -p qnc-app
cargo test -p qnc-app --quiet
```
