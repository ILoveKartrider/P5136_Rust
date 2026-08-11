# P5136 basic-AI difficulty audit

This note records the 2026-08-11 static audit of the ordinary room AI path in
the Korean P5136 client. It distinguishes that path from battle-mode AI,
license duel difficulty, and the server launcher's convenience labels.

## Result

Ordinary room AI difficulty is selected by the server at race start. The
client's add/remove-AI request does not carry a difficulty value. Instead,
`GrCommandStartPacket` contains one six-float race specification for each AI
racer. Changing those values is sufficient to select different AI behaviour
without patching the client, provided the values remain within the native
domain.

The current Rust server does not expose this as a setting. It emits the same
validated specification for every AI:

```text
[0.7, 2400.0, 2950.0, 1.5, 1000.0, 1500.0]
```

The former C# server's **Easy / Hard / Hell** names are server-UI policy, not
an enum recovered from the P5136 room packet. It randomizes the first four
values within three ranges and keeps the last two at `1000` and `1500`.

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

The exact original names of the six values are still unproven. They should
remain indexed fields until their individual `GoBasicAiKart` consumers are
recovered. The first four clearly change pace/behaviour in the retained C#
implementation; assigning names such as acceleration or steering now would be
speculation.

## What the apparent in-game difficulty controls are not

`PqAdjustDuelMissionDifficulty` / `PrAdjustDuelMissionDifficulty` belong to
duel or mission content and are not the ordinary room-AI codec. Likewise, the
client string `******ai speed:%.2f` is reached from an
`AiItemTeamGameParam` path, not from `GrRequestBasicAiPacket`. Neither is
evidence that the room's add-AI button sends an Easy/Hard/Hell enum.

## Safe implementation direction

A future server setting can offer three localized presets and generate one
six-float specification per frozen AI racer, as the C# server did. It should:

1. freeze the selected preset with the room-start snapshot;
2. validate all six finite values before serializing;
3. preserve the AI-count/spec-count equality gate;
4. avoid interpreting the client add/remove `option` byte as difficulty;
5. keep battle, duel-mission, and ordinary room-AI settings separate.

This release documents the boundary only; it does not add a speculative
difficulty selector.
