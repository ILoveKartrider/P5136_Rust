# P5136 basic-AI difficulty audit

This note records the 2026-08-11 static audit of the ordinary room AI path in
the Korean P5136 client. It distinguishes that path from battle-mode AI,
license duel difficulty, and the server launcher's convenience labels.

## Result

Ordinary room AI difficulty is selected by the server at race start. The
client's add/remove-AI request does not carry a difficulty value. Instead,
`GrCommandStartPacket` contains one six-float race specification for each AI
racer. The P5136 client actively consumes the first four values. The last two
are preserved by the packet and `GameParam` codecs but are not read by the
P5136 `GoBasicAiKart` setup path.

The Rust Server management GUI exposes two independently validated vectors and
freezes the applicable one into every AI entry at race start:

```text
speed default = [0.7, 2400.0, 2950.0, 1.5, 1000.0, 1500.0]
item default  = [0.6, 2400.0, 2950.0, 1.5, 1000.0, 1500.0]
```

The former C# server's **Easy / Hard / Hell** names are server-UI policy, not
an enum recovered from the P5136 room packet. It randomizes the first four
values within three ranges, uses a lower field-0 range for item mode, and keeps
the last two at `1000` and `1500`. Rust uses exact operator-entered values
instead of per-racer randomization so one configuration is deterministic.

## Exact packet boundary

| Packet or resource | Recovered data | Difficulty field? |
|---|---|---:|
| `GrRequestBasicAiPacket` | `player_id:u32`, `option:u8` | no |
| `GrSlotDataBasicAi` AI body | character, rider variant, kart, balloon, headband, goggle as six `i16`, then team `u8` | no |
| `GrCommandStartPacket` | counted vector of 24-byte elements; each element is six encoded floats | yes, as behaviour parameters rather than a named enum |
| `zeta_/kr/content/basicAI.xml` | allowed characters, rider variants, accessories, and karts with speed/item eligibility | no |

The request factory constructs a 24-byte object (`0x00724620` ->
`0x00732940`). Two independent live producers, `0x00C47150` and
`0x00CB3B70`, write the requested player ID at object offset `+16` and write
zero to the byte at `+20` before sending it. Other recovered producers leave
that constructor-initialized byte unchanged. This confirms that the ordinary
add-AI UI does not encode an Easy/Hard/Hell choice in `option`; treating it as
a difficulty enum would invent protocol semantics.

The installed executable's `GrCommandStartPacket` reader at `0x0072D970`
passes object offset `0x120` to the counted-vector reader `0x0071DD20`. Its
element reader `0x0071E720` consumes six consecutive encoded floats at offsets
`0, 4, 8, 12, 16, 20`. Writer `0x00730400` and element writer `0x0071F880`
mirror the same shape. This is the native basis for Rust's
`AiRaceSpec([f32; 6])` codec.

## Recovered six-float semantics

The start-command handler copies the packet vector byte-for-byte from packet
offset `+0x120` to `GameParam +0x1D8` (`0x00CF4060` -> `0x00CF57E0`). During
AI creation, `0x00ACEEA0` selects the 24-byte element by the AI's ordinal,
`0x00731A30` copies all six encoded floats, and `0x00952000` applies the race
specification to `GoBasicAiKart`.

| Index | Current server value | P5136 client use | Confidence |
|---:|---:|---|---|
| 0 | `0.7` | One factor in the base target-speed product. The client immediately multiplies it by field 1; the C# presets use this position as the primary difficulty/mode speed coefficient. | exact consumer; policy name inferred |
| 1 | `2400` | The other factor in the base target-speed product. Its native range matches a kart-like forward-force/reference-speed value. The client does not use fields 0 and 1 separately after multiplying them. | exact consumer; original standalone name unproved |
| 2 | `2950` | Timed boost window in milliseconds. It is truncated to an integer, stored at `GoBasicAiKart +0x85C`, and used as the duration of timed behaviour state `3`. | exact |
| 3 | `1.5` | Boost acceleration multiplier. While the timed boost state is active, it multiplies the normal AI acceleration ramp. | exact |
| 4 | `1000` | Copied through the 24-byte codec but never read by the P5136 `GoBasicAiKart` setup or update path. | exact unused/reserved status |
| 5 | `1500` | Copied through the 24-byte codec but never read by the P5136 `GoBasicAiKart` setup or update path. | exact unused/reserved status |

Fields 0 and 1 produce the base speed cap at `GoBasicAiKart +0x614`:

```text
base_target_speed = spec[0] * spec[1]
                  * (item mode ? basicAI.itemVal : basicAI.speedVal)
                  * basicAI.channel_factor
```

The server therefore selects its speed vector for game types 1/3 and its item
vector for game types 2/4. The client then applies the separate mode multiplier
shown in the formula; these are complementary layers, not duplicate fields.

The installed Korean P5136 `basicAI.xml` supplies `itemVal=0.028`,
`speedVal=0.029`, and channel factors such as `S0=0.90`, `S1=0.89`, and
`S2=0.86`. It separately supplies `accel=22`; therefore neither field 0 nor
field 1 should be named simply `accel`. That RHO `accel` value is loaded into
`GoBasicAiKart +0x618` and controls how quickly the simulated speed approaches
the cap.

### Why Rust item-room AI was stationary

The stock Korean P5136 resource defines channel factors for S0, S1, S2, S3,
S4, S6, and S7, but not S5 or S8. The native indexed getter returns `0.0` for
an absent/out-of-range factor. Rust had incorrectly assigned wire speed type 8
to item channels, so an item-room start evaluated the formula above with
`basicAI.channel_factor = 0.0`; the resulting AI target speed was exactly zero.

This was a server mapping error rather than evidence that S8 means integrated
item mode. The stock `channel.xml` explicitly assigns `createSpeed='7'` to
both `itemIndiCombine`/`itemTeamCombine` and
`speedIndiCombine`/`speedTeamCombine`. It distinguishes item and speed racing
with game types 2/4 and 1/3 respectively. Its own comment calls speed type 7
the active integrated-speed channel, while type 4 is the active infinite-
booster speed. `baseStringBag.xml` retains an S8 display key named
`통합속도`, but the active P5136 channel catalog does not bind S8 to item mode.

The server therefore now assigns the stock S7 speed byte to both normal speed
and item channels. Item rooms still select their distinct item-mode physics
matrix row through game type 2 or 4. The nested start-session snapshot also
remains S7, allowing BasicAI to use the defined S7 factor without any AI-only
packet projection.

When boost is selected, `0x009549F0` starts state `3` with `spec[2]` as its
duration. The per-frame update at `0x00952850` then raises the target speed and
multiplies its acceleration increment by `spec[3]`. This also explains why the
C# difficulty presets raise fields 2 and 3 together: harder AI holds boost
longer and accelerates more strongly during it.

The fixed C# values `1000` and `1500` resemble common kart start-booster times,
but P5136 provides no consumer that would justify assigning those names to
fields 4 and 5. They must remain `reserved_4` and `reserved_5` for this build.
Changing them alone cannot change P5136 ordinary-room AI behaviour.

### Related native RHO controls

The same static path recovered the original `basicAI.xml` setting names and
the Korean comments shipped with the client:

| Key | P5136 value | Native purpose |
|---|---:|---|
| `accel` | `22` | AI acceleration |
| `collideFactor` | `0.3` | How strongly the AI is displaced by a collision with a player |
| `conerLow` | `0.73` | Minimum slowdown ratio while cornering (the client contains this spelling) |
| `itemVal` | `0.028` | Item-mode speed conversion factor |
| `speedVal` | `0.029` | Speed-mode speed conversion factor |
| `boosterCheckGap` | `3500` | Speed-mode boost-use decision interval |
| `itemCheckGap` | `4300` | Item-mode item-use decision interval |
| `collideSpeedDown` | `0.6` | Remaining-speed factor after collision |
| `collideMass` | `30.0` | Collision elasticity/mass influence |

These are client-resource parameters, not additional fields in the start
packet. The packet's first four fields and the RHO settings complement one
another in the final AI simulation.

## What the apparent in-game difficulty controls are not

`PqAdjustDuelMissionDifficulty` / `PrAdjustDuelMissionDifficulty` belong to
duel or mission content and are not the ordinary room-AI codec. Likewise, the
client string `******ai speed:%.2f` is reached from an
`AiItemTeamGameParam` path, not from `GrRequestBasicAiPacket`. Neither is
evidence that the room's add-AI button sends an Easy/Hard/Hell enum.

## Implemented server policy

The Server management GUI uses exact values instead of presenting the old C#
Easy/Hard/Hell randomness as a native client feature. The implementation:

1. persists separate speed/item vectors and freezes the mode-selected vector
   with the room-start snapshot;
2. validates all six finite values before server startup, while treating fields
   4 and 5 as preserved compatibility values rather than active controls;
3. preserves the AI-count/spec-count equality gate;
4. does not interpret the client add/remove `option` byte as difficulty;
5. keeps battle, duel-mission, and ordinary room-AI settings separate.

This audit documents the boundary and the four active semantics. The GUI names
are evidence-graded descriptions, not claims that the original native C++
field identifiers have been recovered.
