# qnc-app

Native egui client against `qnc-host` (**no WebView / WASM / browser UI**).

## Run

```powershell
# terminal 1 — API server (also serves legacy web at /app — ignore that)
.\run_host.ps1

# terminal 2 — native egui window titled "QNC App"
.\run_app.ps1
```

Ako vidiš **AI / Virtual / Segment / Export XML** i `127.0.0.1` u browser footeru — to je **web**.  
Native prozor: naslov **„QNC App”**, Host URL red, Project lista s create/templates.

## Done (workflow parity)

| Area | Features |
|------|----------|
| Shell | Connect, ProjectOnly footer, workspace tabs, block workflow without project |
| Project | List/open/delete, create from template, path pickers, workflow tab toggles via host ui-state |
| Ingest | Toolbar chips, Otkrij/Uvezi, poster cards, archive original, select/clear, import poll |
| Story | Toolbar, All/Virtual/Segment, Source/Wrap preview, filmstrip, wrap timeline paint, Mark IN/OUT, covers, Marker@head, commit/Export, keyboard shortcuts |

## Not yet

- Client-local decode / MediaAccess mapping
- Full XML file write (host API; commit is wired like web)
- Custom template save panel
