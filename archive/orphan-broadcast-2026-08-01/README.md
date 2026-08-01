# Orphan Broadcast Archive

Datum arhive: 2026-08-01

Ovo nije aktivni source tree.

Arhiva sadrzi stari `qnc-app` broadcast/native player kod koji nije bio spojen
u `qnc-app/src/main.rs`:

- `qnc-app-src/broadcast/**`
- `qnc-app-src/native_player.rs`
- `qnc-app-src-story/program_builder.rs`
- stare `qnc-app/scripts/test-broadcast-player*.ps1`

Kod smije sluziti samo kao read-only referenca za portanje pojedinih ugovora u
aktivne modularne crateove. Ne vracati ga u `qnc-app/src`.
