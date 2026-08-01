# QNC-timeline (Story form)

**Jedna aplikacija:** Shell + 3 plugin forme (Project, Ingest, **Story**).

U Story formi, **`kodak-timeline`** je univerzalna timeline komponenta:

| Primjena | Schema / model | Što se vidi |
|----------|----------------|-------------|
| **Source timeline** (All tab dock) | `segment_schema: source` | filmstrip, A1, IN/OUT — prazni M/covers |
| **Wrap segment** | `ton` / `off` | + M + pokrivalice |

Razlika = podaci u modelu, ne druga komponenta.

Kod: `app/components/kodak-timeline/` · orchestrator: `plugins/story/static/qnc-story.js` (`sourceTimelineModel`).

## Native modularni model

Native `qnc-app` ne smije graditi poseban “source timeline” i poseban “wrap
segment timeline”. Osnova je jedna univerzalna timeline konstrukcija:

1. **Celulozna traka** = transparentni timecode carrier.
2. **Filmstrip** = opcionalni underlay ispod celulozne trake (`UI postavka`).
3. **Base video** = opcionalni osnovni video sloj; nije uvjet za timeline.
4. **Audio** = 1–4 audio tracka vezana na isti carrier.
5. **Overlay/pokrivalice** = 0..n dodatnih video slojeva.
6. **Markers/effects** = IN/OUT/M i kasnije cut/dissolve/event slojevi.
7. **Z os** = prioritet samo za vizualni/effect stack na istoj celuloznoj
   traci: filmstrip underlay ispod, base video na osnovi, overlays iznad,
   marker/effect najviše. Timecode/celluloid carrier i audio trackovi nisu u Z
   orderu; audio trackovi su laneovi istog vremenskog busa i nisu ovisni o
   vizualnom prioritetu.

`Source` upotreba je samo preset istog modela: carrier + optional filmstrip +
audio trackovi + base video + IN/OUT markeri.

`Wrap` upotreba koristi isti carrier i dodaje slojeve po potrebi: base video,
audio, pokrivalice, M/effect markere i kasnije dodatne efekte.

OFF/VO montaža smije krenuti bez ikakvog video sloja: postoji samo celulozna
traka/timecode + audio laneovi. Prvi video slojevi dolaze tek s
pokrivalicama/overlayima ili eksplicitnim base video slojem.

Audio semantika za montažu:

- A1 = OFF/VO/ton, stalni vremenski bus osnovnog priloga; može imati vlastiti
  level ili mute.
- A2 = audio pokrivalice, aktivan samo u frame rangeu te pokrivalice; svaka
  pokrivalica može imati vlastiti A2 level ili mute.
- Pokrivalica = overlay video sloj; ne postaje base video i ne dira A1.

Implementacijski početak: `qnc-app/src/broadcast/timeline.rs`
(`UniversalTimelineSpec`) i `qnc-app/src/broadcast/graph.rs`
(`BroadcastProgramGraph::from_universal_timeline`).
