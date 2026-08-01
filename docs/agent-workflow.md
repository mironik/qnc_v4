# Agent workflow — prevent multi-agent incidents

Short operational rules for **Codex, Cursor, and other agents** working in this repo.  
Architecture and DB-first contracts live in [development-policy.md](development-policy.md) and [architecture-db-first.md](architecture-db-first.md).

## Before every task

1. **One agent at a time** — only one agent may modify the repo. If another session may be active, stop and confirm with the user.
2. **Clean working tree** — run `git status --short`. It must be **empty** before starting. If not, stop; report what is dirty and ask how to proceed.
3. **Propose a plan first** — list files, scope, and validation steps. **Do not edit source** until the user approves the plan (unless the user explicitly says to proceed in the same message).

## Scope limits

- **One subsystem per task** — at most one plugin or subsystem (e.g. Project tab, ingest, media_pool). No drive-by edits in unrelated areas.
- **No `data/*` commits** unless the user explicitly approves committing runtime data.
- **DB-first is non-negotiable** — SQLite + Rust API is source of truth; plugin JS is snapshot cache only. See [architecture-db-first.md](architecture-db-first.md). Run `scripts/db-first-guard.ps1` as part of validation.

## Before commit

- **`.\test.ps1` must pass** — includes static DB-first guard, integration tests, and subsystem checks.
- **Stage only intended files** — never `git add .` when `data/*` or unrelated files are dirty.
- **Do not amend** unless the user explicitly requests it and the commit was not pushed.

## Before push

- **Restore runtime data** — tests may touch `data/project_store.db`, `data/shell_module_state.json`, and `data/design_overrides/*`. Before push:
  ```powershell
  git restore data/project_store.db data/shell_module_state.json data/design_overrides/timeline-lab.json data/design_overrides/tokens.json
  git clean -f data/design_overrides/*.migrated data/shell_module_state.json.migrated
  ```
- **`git status --short` must be empty** again after restore.
- **Do not push** until the user approves (unless they explicitly asked to push in the same instruction).

## Stash and recovery

- **No `git stash pop` / `git stash apply`** without user review of `git stash show -p` and explicit approval.
- If recovering from a bad session, prefer `git restore` / `git clean` on known paths over applying unknown stashes.

## Quick checklist

| Step | Requirement |
|------|-------------|
| Start | `git status --short` empty; single agent |
| Plan | Approved before source edits |
| Scope | One plugin/subsystem; no unrelated diffs |
| Validate | `.\test.ps1` PASS |
| Commit | Only approved files; no `data/*` unless approved |
| Push | Runtime data restored; status clean; user approved |

## Related

- [AGENTS.md](../AGENTS.md) — architecture and component rules
- [development-policy.md](development-policy.md) — product target and DB-first summary
- `test.ps1` — integration test entry point
- `scripts/db-first-guard.ps1` — static regression guard
