# Documentation map

The documentation was audited as one set on 2026-08-14. User-facing guides
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
| `CHANGELOG.md` | Korean user-facing release history and known limitations |
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

The v1.0.0 documentation baseline retains the v0.3.0 track and ordinary asset
import workflows and adds:

- symmetric server/connector DataRaw flags and a mandatory normalized file-list
  preflight before login when both sides opt in;
- 100 audited experimental XUN import candidates with recovered
  `defaultExceedType` 1-4 and dynamic server inventory publication;
- a source-available, exact-build Win32 XUN sidecar implementing S/B/L charger
  state, six speed-mode physics consumers, continuous charger UI, independent
  aura and ordinary Exceed effects, display conversion, and type-1 server start
  items;
- a private base-port+3 profile channel and per-race generation reset;
- elevated XUN attachment from the Connector tab, independent helper/DLL file
  selection, connector-directory defaults, and a DLL file-logging toggle.

Implementation status is authoritative in `PORTING.md` and
`PORTING_STATUS.md`; the translated READMEs deliberately summarize rather than
duplicate every packet-level caveat.
