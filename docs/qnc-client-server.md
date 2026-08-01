# QNC — klijent / server (plan)

**Status:** dogovoreno (2026-07-16) · **implementacija parkiranа** dok se riješi drugi prioritet  
**Kanonski doc za deployment profile**  
**Povezano:** [qnc-step-plan.md](qnc-step-plan.md), [qnc-playback-engine.md](qnc-playback-engine.md), [architecture-db-first.md](architecture-db-first.md)

---

## 1. Cilj

Jedan **nepromijenjen klijent** u svim okruženjima.  
Ponašanje određuju **projekt + runtime profil**, ne forkovani UI.  
Fizički mediji **ne putuju mrežom** (nema obaveznog uploada).  
SQLite + Rust API = istina projekta.

**Pitch (držati):**  
> Montiraš lokalne snimke. Na server ide projekt (baza) — ne tvoji fajlovi.  
> Bez uploada → manje čekanja, privatnost, povjerljivi materijal kod vlasnika, minimalan promet.

---

## 2. Načela (ne kršiti)

| # | Pravilo |
|---|---------|
| 1 | Univerzalni klijent — isti u svim profilima |
| 2 | DB-first — UI je projekcija SQLite snapshota |
| 3 | Mediji ostaju uz host koji ih vidi — nema cloud storagea kao produkt path |
| 4 | Mrežom samo laki podaci (API, intent, mali preview chunk) |
| 5 | TV kućni ingest / NAS spine **netaknut** — QNC nije novi SPOF kuće |
| 6 | Produkcijska radna jedinica = lokalni `qnc-host` + disk (+ opcionalno house ref) |
| 7 | Online centralna baza = **samo** `online_test` (zaštita projekata testera), ne zamjena za #6 |

---

## 3. Profili

### A — `workstation` (produkcija: teren + TV stanica)

| | |
|--|--|
| Jedinica | Klijent + **lokalni** `qnc-host` + mediji koje stanica vidi |
| Baza | `qnc_project.db` uz taj host |
| Mediji | Teren: lokalni dir / `proxy/`. Kuća: isto |
| Zašto | Isti obrazac kao teren; pad jedne stanice ≠ pad kuće; ne diramo postojeću mrežu |
| Nije default | Više montažera uživo na istom projektu |

### B — `workstation` + house ref (produkcija TV, kasnije)

Isto kao A + MediaResolver:

- `local_file` → projektni / lokalni proxy  
- `house_ingest_ref` → putanja koju stanica već smije čitati  
- Bridge: read-only registracija; **ne** default kopiranje originala u QNC  

### C — `online_test` (QA / dogfood — nije TV produkcija)

| | |
|--|--|
| Baza | **Uvijek na našem online serveru** (projekt ne ostaje kod testera) |
| Mediji | **Nikad obavezni upload** |
| Mod kuća | Seed mediji već na serveru |
| Mod teren | Lokalni fajlovi + **local media agent** (decode lokalno; API → online) |
| Keš | Privremena lokalna baza/snapshot/outbox dozvoljena; istina = serverski SQLite |

```text
[Klijent isti]
  ├─ project / editorial API  →  online qnc-host + SQLite
  └─ play / thumb / filmstrip →  local media agent   (teren)
                              ili server seed disk   (kuća)
```

---

## 4. Mreža — što smije

| Smije | Zabranjeno (produkt) |
|-------|----------------------|
| Auth, project list, SQLite API | Obavezni upload originala/proxyja |
| Intent → write → snapshot | Centralno skladište tuđih snimaka kao default |
| Mali preview s hosta koji vidi disk | Cloud decode tuđeg diska bez agenta |
| Virtual-source meta / putanja | Tihi sync cijelog `proxy/` foldera |

---

## 5. Arhitektura (cilj)

```text
 QNC Client (isti)
        │
        ▼
 Project / editorial API  ──►  SQLite (istina)
        │
 MediaResolver (local_file | house_ingest_ref)
        │
   ┌────┴────┐
   ▼         ▼
 lokalni   house / seed
 disk+agent  disk
```

Playback: proxy-first u Rustu na stroju koji vidi medije — vidi [qnc-playback-engine.md](qnc-playback-engine.md).

---

## 6. Logički redoslijed faza

```text
P0 ugovori (doc)
  → P1 workstation referent     ← prvo (istina montaže)
  → P2 online_test no-upload    ← testeri / pitch
  → P3 TV workstation + refs
  → P4 dijeljeni projekt        ← opcionalno, ne default
```

### P0 — Ugovori

| ID | Deliverable | Status |
|----|-------------|--------|
| P0.1 | Ovaj doc (profili A/B/C) | ✓ |
| P0.2 | `docs/qnc-virtual-source.md` | pending |
| P0.3 | Shell capabilities: `profile`, `db_remote`, `media_mode`, `upload_required:false` | pending |
| P0.4 | No-upload / pitch u development-policy | pending |

### P1 — Workstation referent (kritični put)

| ID | Akcija | Veza na step-plan |
|----|--------|-------------------|
| P1.1 | Lokalni host + klijent, jedan laptop | F2–F5 |
| P1.2 | Ingest lokalni dir → proxy + filmstrip + Story source | Ingest + Story |
| P1.3 | Univerzalne komponente: player surface + qnc/source timeline (`update(model)`) | components SDK |

**Done:** montaža radi offline na jednoj stanici.

### P2 — Online test (baza remote, mediji ne putuju)

| ID | Akcija |
|----|--------|
| P2.1 | Online `qnc-host` + centralni project store |
| P2.2 | Auth + project ACL (min.) |
| P2.3 | Mod kuća-seed (mediji na serveru) |
| P2.4 | Mod teren: local media agent; editorial → online |
| P2.5 | Lokalni snapshot keš + outbox (nakon P2.4) |
| P2.6 | UI bez upload patha; opcionalno metrics prometa |

**Done:** lokalni folder bez uploada **ili** seed „kao kuća“, isti klijent.

### P3 — TV kuća (isti model kao teren)

| ID | Akcija |
|----|--------|
| P3.1 | `workstation` po montažnom PC-u |
| P3.2 | `house_ingest_ref` + Bridge watch (read-only) |
| P3.3 | Handoff teren→kuća (db + proxy paket / re-link) |

### P4 — Namjerno dijeljenje (kasnije)

Više montažera na istom projektu uživo = eksplicitni centralni host **ili** sync.  
Ne miješati s P2; ne forsirati kao SPOF kuće.

---

## 7. Runtime capabilities (skica)

```json
{
  "profile": "workstation | online_test",
  "db_remote": false,
  "media_mode": "local_host | local_agent | server_seed",
  "upload_required": false
}
```

| Profil | db_remote | media_mode |
|--------|-----------|------------|
| workstation | false | local_host |
| online_test + teren | true | local_agent |
| online_test + kuća | true | server_seed |

---

## 8. Rizici

| Rizik | Pravilo |
|-------|---------|
| Keš = druga istina | Keš je projekcija; write na kanonsku bazu |
| Online = „produkcija“ | Odvojeni server + label Test/dogfood |
| Agent down | `media_agent_unavailable` — nikad tihi upload |
| „Jedan QNC za cijelu kuću“ | Samo P4, ne P3 default |

---

## 9. MVP

**P1:** laptop, lokalni host+db+proxy, play + All → player/source timeline.  

**P2:** (1) seed na serveru, (2) lokalni folder + agent + remote DB, (3) isti klijent, 0 upload.

---

## 10. Stanje rada

| | |
|--|--|
| Plan | **dogovoren** |
| Kod za P2/P3/P4 | **ne kreće sada** |
| Nastavak | Nakon drugog prioriteta: P0.2 (`qnc-virtual-source.md`), zatim P1 |

Kad se vratiš na ovaj track, prvi commit-able korak je P0.2 + uskladiti capabilities skicu s `GET /api/shell/runtime`.
