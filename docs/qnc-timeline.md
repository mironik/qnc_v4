# QNC Timeline

Timeline je UI komponenta i daljinski prikaz/upravljac. Nije dio broadcast
player corea.

## Pravila

- Frame je istina.
- Sekunde su samo izvedeni UI prikaz.
- Timeline ne dekodira media.
- Timeline ne cita filmstrip kao playback izvor.
- Timeline ne posjeduje transport.
- Timeline ne zna za Story, MA, Ingest ili Project kao vlasnike toka.
- Timeline prima neutralne frame/range/layer modele i prikazuje ih.
- Komande koje korisnik okida na timelineu moraju ici kroz neutralni command
  ugovor, ne direktno u form-specific kod.

## Odnos S Playerom

Broadcast player emitira neutralne evente, npr. `CarrierPositionChanged`,
`TransportStatusChanged` i `PlaybackBoundaryReached`.

Timeline je projekcija tih podataka. Ako je potreban follow-up nakon boundary
eventa, to nije skrivena logika timelinea ni player corea; to mora biti vanjski
neutralni command/automation sloj koji salje novu komandu.

## Filmstrip

Filmstrip je UI background/orijentacija. Nema izvrsnu vezu s playerom,
transportom, decodeom ili timebase matematikom.
