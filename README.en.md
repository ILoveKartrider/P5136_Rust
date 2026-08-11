# P5136 Rust Server

[한국어](README.md) | [English](README.en.md) | [简体中文](README.zh-CN.md)

If any translation sounds unnatural or is incorrect, please [open an issue](https://github.com/ILoveKartrider/P5136_Rust/issues) or submit a pull request.

This is an independent Rust server and connector for the Korean KartRider P5136 client. The original C# project is used only as a read-only protocol reference and is not included in this repository.

This project is not yet a complete recreation of every commercial service feature. Its current goal is a stable multiplayer cycle for friends on the same LAN: create or join a room, start a race, drive, view results and the ceremony, and return to the room. Lucci, bonus items, team flags, and some event, social, and shop features are out of scope or receive limited responses.

See the [documentation map](DOCUMENTATION.md) for the current roles of every user guide, porting ledger, client FSM, and packet, item, or AI audit.

## Quick start

Build a release with Rust 1.94 or newer:

```powershell
cargo build --release -p p5136-cli
```

A legacy-compatible Windows Server 2012 x64 build uses the same source with a separate target and static CRT. See [the Windows Server 2012 build notes](WINDOWS_SERVER_2012.md) for the procedure and validation scope.

The repository currently uses this fixed output path:

```text
target/p5136-finish-kart-abilities/release/p5136.exe
```

Run `p5136.exe` without arguments to open the GUI. Choose 한국어, English, or 简体中文 in the upper-right corner; the selection is restored on the next run. The server and connector are two tabs in the same executable.

1. In the Server tab, set the required **Client or Profile path** to the P5136 client root, for example `C:\Games\KartRider_5136`.
2. If another PC will connect, select **Auto-configure my LAN IPv4** and choose the real LAN adapter when several are listed.
3. Enable **Allow new nicknames on LAN** when a remote nickname connects for the first time.
4. Select **Start server**.
5. In the Connector tab, enter the game directory, nickname, and server IPv4, then select **Prepare and launch client**.

The connector prepares PIN/XML using an immutable pristine backup and a process lock, then launches through Windows UAC, Wine, CrossOver, or a macOS Sikarugir wrapper. For manual prefix and wrapper setup, see the [macOS Sikarugir walkthrough](MACOS_SIKARUGIR.md).

## Automatic client-data discovery

You may select the client root, `Profile`, or `Data` directory. The server locates `Data` and parses these sources directly with bounded, read-only readers. The C#-generated `Profile/KartCatalog.xml` is not required.

- `Data/kart.rho` plus RHO5 overlays: kart names and driving physics
- RHO5 `zeta_/kr/shop/data/item.kml`: inventory catalog
- `Data/item.rho`: per-kart item transforms and solo/team probability tables
- `Data/track_common.rho`: track pools by mode and random selector
- Other RHO5 data: emblems and other supported catalogs

The default item-probability mode is **Apply automatically at server start**. Starting the server first reads the actual `item.rho`, reports the solo/team entry counts and source in the GUI, and passes that exact immutable snapshot to the server run. A load failure prevents startup. Values become path-independent only after **Load and pin** or an XML override is used.

Kart item transforms such as Sebec V1 are also read from `item.rho` `transformByKart`; the server determines the final acquired item. For example, Sebec V1's gold shield is a server transform rather than an automatic client transform. The base and Korean `animalBooster` tables are merged separately, so special-booster karts such as Pharaoh HT and Bastet X receive item 31 and let the client display their kart-specific Gold Booster. Gold Shield and Gold Booster remain independent effects. The catalog is immutable in memory and never modifies RHO archives or the client directory.

## Per-nickname kart inventory

Expand **Inventory editor by nickname** to grant multiple copies of one kart with separate enhancement state. The operation is serialized through the same profile queue while the server is running; reconnect clients that were already online.

1. Set the server's client path and profile storage path.
2. Select **Load kart list** to read real names and IDs from client RHO data.
3. Enter the nickname to edit, or copy the current connector nickname.
4. Search with a name, a whitespace-normalized name, or a numeric ID such as `1410`, then select a result.
5. Select **Add selected kart**. Repeating the action allocates another unique serial.

Only karts with a resolved name, `BodyParam`, actual `model.1s`, and no test/dummy/NPC indicators are granted automatically as serial 1. In the stock P5136 data, 1,284 of 1,296 karts pass. Twelve IDs (`199, 312, 323, 352, 657, 658, 659, 744, 745, 746, 814, 886`) remain quarantined to avoid inventory-scroll crashes. If a quarantined kart is confirmed to work in the client, enter its exact numeric ID and select the **manual review** result. This adds only a serial-2-or-higher copy for that nickname; it does not re-enable the default serial-1 grant.

Additional copies are stored atomically in the nickname profile's `GrantedKarts`. They use the same `(kart_id, serial)` key as tune, plant, level, and parts data, so each copy can keep different upgrades. The allocator also reserves serials still referenced by `TuneData.json`, `PlantData.json`, `LevelData.json`, and `PartsData.json`. Missing profiles are created on the first grant. Reconnect a client that was already online.

Legacy-engine upgrades use a simplified friends-server policy. The target and material karts must be owned, but materials and currency are not consumed and success is always 100%. Level, remaining points, four-slot distribution, and special effects are saved per nickname in `LevelData.json` and restored into inventory and race physics. Each slot is limited to 0–10 and the total distribution to 35 points.

The stock floater UI is supported for socket creation, activation kits, protection spanners, and reset. Consumables are not deducted, but ownership, kart type, and socket state are validated and saved atomically in `TuneData.json`. Black-kit values `603/703/903` contribute start-booster time `+800`, transform acceleration `+0.018`, and drift escape force `+210`. Normal speed values `103`–`903` follow the C# values. All 20 item Floater meanings are derived from the Korean P5136 RHO5 `zeta_/kr/enchant/desc.xml`. Item-box transforms—shield to super shield, water bomb to toxic/ice water bomb, rocket to gold rocket, booster to siren/super shield, and banana to water mine—run in the server award path. Every chance comes from its exact `enchant.xml` Tune entry (for example, `10503` 15%, `11103` 40%, `11403` 25%, and `11803` 30%); RHO replacement effects are 100%. In contrast, the client resolves and awards all 78 `firing2Gain` use-trigger rewards and 150 `fired2Gain` hit-trigger rewards itself. The server relays the corresponding GameSlot types 10 and 11 byte-exactly and does not synthesize duplicate rewards.

When granting a kart in the GUI, use the existing **333 preset** or select each of the three vertically arranged Floater slots. The editor exposes 27 validated speed codes and all 20 RHO-verified item effects by their actual translated meanings. It rejects duplicate speed-effect families and duplicate item codes before creating the kart. **Apply grade 5** remains independent and can be combined with either Floater choice.

The default inventory keeps Kartneck/Kartneck X as verified P5136 exceptions even though their historical internal names contain `dummyBox`. Boxter HT-S/HT-B/HT LE are quarantined again because their shared, incomplete model layout behaves like dummy data in the stock client. Four bodies without a Korean/default BodyParam and five explicit dummy/test bodies also remain excluded.

The editor briefly acquires the same profile-root lease used by the server and revalidates the store identity. A server in another process therefore blocks offline edits. Live grants use the running server's serialized profile queue. If the client path changes after a catalog is loaded, the catalog and selection are invalidated and the canonical `Data` path is checked again immediately before a grant.

## Random tracks

The server parses the client's RHO 1.0 `track_common.rho` read-only. It selects from the real pools for speed/item modes and selectors `0, 1, 3–8, 23, 30, 33, 40`, without repeating a track until the current room pool is exhausted. AI rooms prefer `basicAi` tracks and fall back to the original pool when necessary.

Load the catalog in **Random-track settings** to edit each pool with checkboxes. **Select all**, **Clear all**, and **Client defaults** are available. **Apply to running server** replaces all pools atomically for the next race while leaving loading or active races unchanged. Empty custom pools are rejected before application or startup. Manual overrides require the configured client `Data` directory so IDs can be validated.

Track and pool proper names come from the Korean client data and are therefore shown in the client's original language even when the GUI chrome is English or Simplified Chinese.

## Static audit of AI difficulty

The ordinary room add/remove-AI request contains no difficulty value. Actual AI behaviour parameters arrive in `GrCommandStartPacket` as six encoded floats per AI racer. **AI driving parameters** under Server management exposes separate speed and item vectors, defaulting to `[0.7, 2400, 2950, 1.5, 1000, 1500]` and `[0.6, 2400, 2950, 1.5, 1000, 1500]`. The GUI persists them and applies them to races after the server is restarted. The former C# launcher's Easy, Hard, and Hell labels were server presets that randomly generated these values from mode-specific ranges, not a native client enum.

The recovered consumer path shows that the first two values are multiplied into the AI's base target speed, the third is the boost duration in milliseconds, and the fourth is the boost-acceleration multiplier. The fifth and sixth values pass through the P5136 codec but are not read by `GoBasicAiKart`; they are reserved compatibility fields in this build. The exact formula and the complementary client-side `basicAI.xml` parameters are documented in [the AI difficulty audit](AI_DIFFICULTY_AUDIT.md). Duel-mission difficulty and battle-mode AI are separate paths.

## S0–S8 room-title physics

An independent `S0`–`S8` token in the room title selects that modern C# physics preset for the next race:

**Time-attack physics preset** under Server management offers **Default (client selection)** plus S0–S8. Default preserves the existing client-requested grade. Selecting an S grade overrides the 235-byte physics block in every accepted time-attack start reply after the server is restarted.

```text
[S0] Beginner
Friendly S2
S4 Infinite Booster
```

Tokens are case-insensitive and use ASCII alphanumeric boundaries, so `TESTS1ROOM` and `S10` do not match. Updating the room title broadcasts the title and password immediately; the physics token applies at the next race start. Without a token, both normal speed and item channels use the integrated S7 selected by the stock P5136 `channel.xml`, while solo/team Infinite Booster uses the regular S4 preset. Item individual/team rooms select their distinct item-physics rows through game types 2/4 rather than a different speed byte. S6 remains a special event preset. S8 retains an `integrated speed` display key and physics slot in the client but is not bound to an active stock channel, so it is available only as an explicit manual preset. Each player receives a 235-byte race-start physics block composed from the room default and that player's kart, pet, and equipment.

## Account roles: observer and anonymous league

Enable **Observer mode (pmap 718)** in the Connector tab to request the observer-master launcher profile. The server saves that nickname's `pmap` atomically; disabling the option and reconnecting restores the regular value `0`. As in C#, pmap 718 enters observer slots 8–15 while retaining the real `RoomMaster` role, so it can change maps and start races after regular riders join. This authority is not automatically given to the regular observer pmap 590.

Observer chat uses `GrRiderEchoPacket` with observer IDs 8–15 and is sent to every other room member. Rust regression tests verify frame delivery to regular riders; a simultaneous observer/regular live capture is still useful for checking exact stock-client presentation.

**Anonymous league mode (pmap 1798)** is a mutually exclusive connector checkbox. Static analysis resolves 1798 as `0x400|0x200|0x100|0x4|0x2`; in the recovered league-ready path it projects opponents to shared character, color, and equipment values. No client branch was found that rewrites the nickname string itself, so the server does not currently anonymize names. The extra bits may affect other UI paths, and 1798 is not treated as a simple alias for 1068. Disabling the option and reconnecting persists pmap 0 again.

## Teams and the next starting grid

New racers in a team room join the team with fewer members. Ties choose Blue first, producing Blue, Red, Blue, Red for the initial entries. Physics slots remain Blue 0–3 and Red 4–7, with AI included in team counts.

After settlement, the server stores the complete confirmed order, including DNF, in each waiting-room `RoomPlayer.ranking`. The next `GrCommandStart` serializes those values, so the previous result feeds the next starting grid. Departed racers are removed while preserving the relative finish order and compacting positions from zero.

The room master is also reassigned after returning to the lobby. Solo mode chooses the highest-ranked remaining human racer; team mode chooses the highest-ranked remaining member of the confirmed winning team. AI, observers, and departed racers are excluded.

## LAN addresses and domains

The bind address is the local interface on which ports open. The advertised IPv4 is written into login and channel packets. Automatic LAN setup excludes loopback and link-local addresses, prefers physical over virtual adapters, and prioritizes private IPv4 ranges. When Wi-Fi, Ethernet, WSL, VMware, VPN, Tailscale, and similar adapters coexist, select the address on the same network as the clients. `0.0.0.0`, multicast, and broadcast advertised addresses are rejected.

Bind, advertised, and connector server fields currently accept IP literals only. The advertised address is serialized as four IPv4 bytes, so a domain name cannot be placed directly into the P5136 packet. Resolve a domain to one fixed A-record address before startup if needed.

The GUI persists server/connector inputs in the operating-system app storage: addresses, ports, paths, nickname, runner, Wine/CrossOver/Sikarugir values, advanced limits, file-logging choice, edited probability tables, random-track selections, and the GUI language. Runtime state, logs, loaded catalogs, and temporary search results are not persisted.

Windows loads Malgun Gothic, Microsoft YaHei, and SimSun when available. macOS loads Apple SD Gothic Neo, PingFang, and AppleGothic. Linux searches Noto Sans CJK and Nanum Gothic families. A warning is logged when no suitable CJK font is found.

With base port `39311`:

| Purpose | Protocol | Port |
|---|---:|---:|
| Game | UDP | 39311 |
| Login | TCP | 39312 |
| P2P/relay | UDP | 39312 |
| Messenger | TCP | 39313 |

Allow these TCP/UDP ports through the server PC firewall when testing from two machines.

## Command-line use

Run only the server:

```powershell
p5136.exe server `
  --bind 192.168.1.10 `
  --advertise 192.168.1.10 `
  --client-dir C:\Games\KartRider_5136 `
  --allow-remote-profile-creation
```

Run only the connector:

```powershell
p5136.exe connect `
  --game-dir C:\Games\KartRider_5136 `
  --username player1 `
  --server 192.168.1.10
```

Add `--observer` for the observer-master profile or `--anonymous-league` for pmap 1798. The two flags conflict.

macOS Sikarugir example:

```bash
p5136 connect \
  --game-dir "/Users/player/Games/KartRider_5136" \
  --username player \
  --server 192.168.1.10 \
  --runner sikarugir \
  --sikarugir-app "/Users/player/Applications/Sikarugir/kartrider.app"
```

Use `p5136.exe --help`, `p5136.exe server --help`, and `p5136.exe connect --help` for all options.

## Logs and troubleshooting

Each run creates a new file next to the executable:

```text
logs/p5136-<timestamp>-<pid>.log
```

The GUI shows the absolute log path. Received and transmitted packets are written to the detailed file log by default. Unknown packets are recorded with bounded raw bytes and consumed without a reply so one unsupported menu does not terminate the complete login session.

Keep both the server log and the client's `logs` directory when investigating a client crash.

## Tests

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Run the real-client RHO smoke test separately:

```powershell
$env:P5136_CLIENT_DATA_DIR='C:\Games\KartRider_5136\Data'
cargo test -p p5136-server configured_real_client_catalog_matches_the_known_p5136_shape -- --nocapture
```

Protocol evidence, completed scope, and continuation points are documented in [PORTING.md](PORTING.md), [PORTING_STATUS.md](PORTING_STATUS.md), [CLIENT_PROTOCOL_FSM.md](CLIENT_PROTOCOL_FSM.md), and [ITEM_GAMEPLAY_COVERAGE.md](ITEM_GAMEPLAY_COVERAGE.md).

## Safety and license

The workspace enforces `unsafe_code = "forbid"`. External files are processed by bounded read-only parsers, and profile persistence uses temporary files and atomic replacement.

License: `AFL-3.0`.
