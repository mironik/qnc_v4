# QNC Host (Rust)

Multiplatform **API** server — Shell API v1, bez Pythona, **bez web UI**.

Native UI = `qnc-app`. Legacy web = `web-arhive/` (nije mountan).

## Zahtjevi

- [Rust](https://rustup.rs) (rustup)

## Pokretanje

```powershell
cd C:\Users\miron\Projects\QNC
.\run_host.ps1
```

API: **http://127.0.0.1:8001/api/health**

## Env

| Varijabla | Default | Opis |
|-----------|---------|------|
| `QNC_ROOT` | auto-detect | Korijen s `seed/`, `qnc-host/`, `qnc-app/` |
| `QNC_API_PORT` | `8001` | Port (ili `data/shell_config.json`) |
| `QNC_BIND_HOST` | `127.0.0.1` | HTTP bind; `0.0.0.0` za LAN (zahtijeva `QNC_TRUSTED_LAN=1`) |
| `QNC_TRUSTED_LAN` | (off) | Dozvoli non-loopback bind. FS browse/pick/delete nemaju auth — internet nije podržan bez vanjskog auth/proxy. |
| `QNC_PROJECTS_ROOT` | OS user data dir | Globalni fallback za projektne foldere |
| `QNC_APP_VERSION` | `host-0.1` | Vidljivo u runtime API |
| `QNC_DEPLOYMENT` | `portable` | Label okruženja |

## Seed (host-owned)

| Put | Uloga |
|-----|--------|
| `seed/system_seed.json` | System project templates |
| `seed/keyboard-shortcuts.json` | Keyboard catalog |
| `seed/tabs/*/plugin.json` | Tab manifeste (bez JS) |
| `seed/components/registry.json` | Prazan registry (API compat) |

## Shell API (v1)

- `GET /api/health`
- `GET /api/shell/runtime`
- `GET /api/shell/diagnostics`
- `GET /api/shell/tabs`
- `GET /api/shell/components`
- `GET /api/shell/keyboard-shortcuts`
- `POST /api/shell/components/sync` (MVP no-op)
- `GET /api/modules`
- Project / Ingest / Story API…

SQLite: `data/project_store.db`; po projektu `…/qnc_project.db`.
