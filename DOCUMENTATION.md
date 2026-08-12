# Documentation map

The documentation was audited as one set on 2026-08-12. User-facing guides
are translated into Korean, English, and Simplified Chinese. Protocol and
reverse-engineering ledgers remain in English so packet names, evidence grades,
and source references stay unambiguous.

If a translation is inaccurate, please open a GitHub issue or pull request.
Do not attach proprietary game binaries, original RHO archives, account data,
or decompiler databases.

## User guides

| Document | Purpose |
|---|---|
| `README.md` | Korean setup, features, account roles, LAN, CLI, and troubleshooting |
| `README.en.md` | English version of the user guide |
| `README.zh-CN.md` | Simplified Chinese version of the user guide |
| `WINDOWS_SERVER_2012.md` | English legacy-target/static-CRT build guide; source changes are not required |
| `MACOS_SIKARUGIR.md` | Korean manual Wine/Sikarugir wrapper setup |

## Implementation and protocol ledgers

| Document | Purpose |
|---|---|
| `PORTING.md` | concise completion checklist and compatibility invariants |
| `PORTING_STATUS.md` | chronological, resumable engineering handoff |
| `CLIENT_PROTOCOL_FSM.md` | protocol-visible client stages and legal transitions |
| `CLIENT_CONSUMER_AUDIT.md` | native server-packet consumer census and independent-oracle gaps |
| `CAPTURED_PACKET_COVERAGE.md` | retained trace boundary and packet classification |
| `ITEM_GAMEPLAY_COVERAGE.md` | item terminology mapped to recovered P5136 operations |
| `AI_DIFFICULTY_AUDIT.md` | ordinary room-AI request/start codecs and difficulty ownership |
| `ASSET_CONVERSION_CANDIDATES.md` | deployed-client CN-to-P5136 static candidate census after the ordinary-V1 Exceed correction |

The fixture note under
`crates/p5136-client-oracle/tests/fixtures/README.md` documents synthetic
oracle bytes. Those fixtures contain no captured account or authentication
data.

## Current release boundary

The v0.3.0 documentation baseline includes:

- stock S7 integrated physics for normal speed/item channels and stock S4 for
  Infinite Booster, with S8 retained as a manual-only client physics slot;
- validated, persistent, separately configurable speed/item basic-AI vectors
  selected by room game type at race start;
- atomic room departure when a racer enters MyRoom, Shop, Magic Hat, Club,
  Rider School, Time Attack, Challenger, Scenario, Single Player, or Matching;
- conservative client-catalog quarantine with Kartneck retained and the three
  incomplete Boxter variants hidden again;
- regular pmap 0, observer-master pmap 718, and anonymous-league pmap 1798
  connector presets;
- a reversible connector-side Korean track-locale overlay for the three
  complete TF tracks and six statically validated blocked tracks, with
  incomplete XYY and story-only `S` stages kept excluded;
- a selectable KR/CN external-client I/R track importer that merges
  catalog/locale rows, supplies AI and selector aliases, resolves guarded
  track-local/cross-theme material dependencies, audits positional environment
  sounds and theme BGM, reports phase/file progress, maps the reviewed
  Fengshen theme through P5136's dormant native XYY selector slot, mirrors its
  theme resources into the XYY runtime namespace used for bare materials, and
  installs only into a complete DataRaw tree without overwriting
  existing same-path P5136 resources; installed tracks can be selected again
  for an idempotent dependency/catalog repair;
- the static finding that pmap 1798 activates the recovered client-side shared
  character/color/equipment projection, but does not itself anonymize
  nicknames on the server;
- the static basic-AI difficulty boundary and four active field semantics
  described above.

Implementation status is authoritative in `PORTING.md` and
`PORTING_STATUS.md`; the translated READMEs deliberately summarize rather than
duplicate every packet-level caveat.
