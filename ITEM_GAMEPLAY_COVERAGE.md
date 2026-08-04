# P5136 item gameplay-reference coverage

## Boundary

This ledger covers every one of the 54 item headings in the supplied Korean
page `크레이지레이싱 카트라이더/아이템` (last edit shown by the page:
`2026-07-11 00:02:47`, attachment SHA-256
`51501a82e6d78a759270d69eea1bde08eda5bb77db91fb95cd02590e73e22d1b`).

The page is a gameplay terminology and target/effect hint, not P5136 wire
evidence. It spans later game versions, so its durations, probabilities,
availability, defense interactions, and modern balance behavior are not copied
into the server. The executable-backed type-12 schema/semantic ledger remains
authoritative for packet offsets and states.

The machine-readable source is
`p5136_core::item_gameplay_catalog::P5136_GAMEPLAY_ITEM_HINTS`. It records all
54 Korean names, a stable slug, category, target scope, effect summary, 41
currently proven P5136 numeric/name links, and evidence-graded `Gop*` links.
The 54 exact `(heading, category, targets, effects)` rows and the ordered
41-pair ID manifest are pinned by literal tests rather than regenerated from
the production table.

The 41 numeric/name links consist of 19 retained fallback-table pairs, 20
Korean-executable initializer pairs, and two independently verified profile
supplements (`siren=24`, `superMagnet=103`).

Legend:

- `verified`: the P5136 native writer establishes the class and the page
  heading association is direct;
- `named`: P5136 class exists, but page-to-class association is name/effect
  correlation only;
- `ambiguous`: two meanings or client generations cannot yet be distinguished;
- `scope deferred`: retained for complete page coverage, but intentionally not
  part of the current reverse-engineering queue;
- `—`: gameplay entry is covered, but no honest P5136 operation link exists.

## Complete 54-item ledger

| Category | Page item | P5136 item symbol/ID | `Gop*` link | Status |
|---|---|---|---|---|
| Acceleration | 부스터 | `booster=6` | — | ID only |
| Acceleration | 파워 부스터 | — | — | gameplay reference only |
| Acceleration | 사이렌 | `siren=24` | `GopSiren` | verified |
| Acceleration | 쭝쯔 | — | — | gameplay reference only |
| Acceleration | 자석 | `magnet=5` | `GopMagnet` | verified |
| Acceleration | 황금 자석 | `superMagnet=103` | `GopSuperMag` | verified |
| Attack | 미사일 | `rocket=7` | `GopRocket` | verified |
| Attack | 1등 미사일 | `guideRocket=33` | `GopRocket` | verified; state-1 `item_id:u16@16` selects 33 |
| Attack | 로켓포 | — | `GopRocket` | named |
| Attack | 황금 미사일 | `goldRocket=32` | `GopGoldRocket` | verified |
| Attack | 호랑이 미사일 | `tigerRocket=99` | `GopTigerRocket` | verified |
| Attack | 전자기 미사일 | `lockdownRocket=104` | `GopLockdownRocket` | verified |
| Attack | 눈의 요정 | `snowman=112` | `GopSnowman` | verified |
| Attack | 랜덤 미사일 | — | — | gameplay reference only; no P5136 join proven |
| Attack | 물폭탄 | `waterBomb=9` | `GopWaterbomb` | verified |
| Attack | 자폭(시한) 물폭탄 | `timeBomb=13` | `GopTimebomb` | verified |
| Attack | 독성 물폭탄 | `infectedBomb=27` | `GopInfectedBomb` | verified |
| Attack | 코-크 폭탄 | `cokeBomb=20` | `GopCokebomb` | verified |
| Attack | 얼음폭탄 | `snowBomb=34` | `GopSnowbomb` | verified |
| Attack | 롤링 물폭탄 | `rollingCokeBomb=22` | `GopRollingbomb` / `GopRollingCokebomb` | scope deferred; ID verified |
| Attack | 그물 | — | — | gameplay reference only |
| Attack | 물파리 | `waterFly=4` | `GopWaterfly` | verified |
| Attack | 얼음 물파리 | `snowWaterFly=118` | `GopSnowWaterfly` | verified |
| Attack | 독성 물파리 | `infectedWaterFly=119` | `GopInfectedWaterfly` | verified |
| Attack | 폭탄 물파리 | `waterbombFly=120` | `GopWaterbombFly` | verified |
| Attack | 우주선 | `ufo=3` | `GopUfo` | verified |
| Attack | 우주모함 | — | `GopAreaUfo` | named |
| Attack | 벼락 | `thunderbolt=111` | `GopThunderbolt` | verified |
| Defense | 실드 | `shield=10` | `GopShield` | state 1 activation; state 2 non-terminal defense impact |
| Defense | 천사 | `angel=11` | `GopAngel` | state 0 timed team activation; state 2 repeatable defense impact, not consumption |
| Defense | 슈퍼 실드 | `superShield=18` | `GopShield` | verified; state-1 `item_id:u16@16` selects 18 |
| Defense | 황금 실드 | `goldShield=36` | `GopGoldShield` | state 0 kind 0 activation; state 2 repeatable defense impact |
| Defense | 프로텍트 실드 | `protectShield=81` | `GopGoldShield` | state 0/2 kind 3; exact shared codec |
| Defense | 사이렌 실드 | `sirenShield=106` | `GopSirenShield` / `GopGoldShield` | own effect plus `GopGoldShield` state-2 defense override 106 |
| Defense | 전자파 | `emp=12` | `GopEmp` | verified |
| Placement | 먹구름 | `darkCloud=1` | `GopCloud` | verified |
| Placement | 먹물구름 | `darkCloud2=115` | `GopCloud2` | verified; discriminator 3 |
| Placement | NEW 구름 | `cloud2=114` | `GopCloud2` | verified; discriminator 0 |
| Placement | 요정 구름 | `rainbowCloud=43` | `GopCloud` | verified; discriminator 6 |
| Placement | 바나나 | `banana=8` | `GopBanana` | verified |
| Placement | 대왕 바나나 | `bigBanana=85` | `GopBanana` | named family link |
| Placement | 지뢰 | — | `GopMine` | verified |
| Placement | 오리폭탄 | `duckMine=45` | `GopMine` | named family link |
| Placement | 물지뢰 | `waterMine=37` | `GopWaterMine` | verified |
| Placement | 부비트랩 | — | `GopForceZone` | named |
| Placement | 바리케이드 | `barricade=113` | `GopBarricade` | verified + retained LAN trace |
| Status | 대마왕 | `devil=2` | `GopDevil` | verified |
| Status | 닥터 R | — | `GopDrmad` | named; bounded C# relay-only class |
| Status | 강시 | — | `GopMqDevil` / `GopNewDevil` | scope deferred |
| Status | 1위 대마왕 | — | `GopMqDevil` / `GopNewDevil` | scope deferred |
| Status | 자물쇠 | `slotLock=110` | `GopSlotLock` | verified |
| Utility | 스캐닝 | `scanning=109` | `GopScanning` | verified |
| Utility | 고스트 | — | `GopGhost` | verified |
| Utility | 검은 기름 | — | `GopOil` | verified |

## Recovered joins and remaining boundary

The active ambiguous joins are now closed. The decisive client-side evidence
is:

1. `GoItemRocket::sub_9D25A0` fixes item 33 and emits `GopRocket`; its state-1
   body is `item_id:u16@16, token@18, source@22, ...`. `GopStraightRocket`
   instead hard-codes item 73, and `straightRocket/item.bml` names the resource
   `거대미사일`.
2. `timeBomb/item.bml` is the normal timed-waterbomb resource and matches
   `GopTimebomb`. `GopBigTimebomb` hard-codes item 122, while its BML names the
   effect `어비스배틀팀필살기`; it is a separate battle-team special.
3. `snowWaterFly/item.bml` contains the ordinary enhanced ice-waterfly states
   and matches item 118 / `GopSnowWaterfly`. `GopIcefly` is fixed item 80 and
   `iceFly/item.bml` names the Kefi special (`케피필살기`).
4. `GoItemShield::sub_9E0470` branches on item 18 and emits `GopShield` with
   `item_id:u16@16`; `GopSpecialShield` hard-codes item 40 and its BML contains
   Defense Kit / Autobot-special effects.
5. Native Cloud producers map discriminator `0/3/6` to item IDs `0/1/43` for
   `GopCloud` and `114/115/116` for `GopCloud2`. This fixes Fairy Cloud to
   `GopCloud`, not `GopCloud2`.

Rolling Waterbomb, Jiangshi, and first-place Devil are deliberately marked
`DeferredByUser`; they remain visible but are excluded from active ambiguity
counts. Net and modern Random Missile have no proven P5136 ID/consumer join.
That is an absence-of-evidence boundary, not a competing packet mapping.

The page does not contain the separate P5136 items Giant Missile (73), Abyss
battle-team special (122), Kefi special (80), Special Shield/Autobot special
(40), or Rainbow Cloud 2 (116). Their exact associations are nevertheless kept
in `P5136_RECOVERED_OPERATION_ITEM_ASSOCIATIONS` to prevent those names from
being reintroduced as candidates later.

The gameplay model distinguishes the immediately preceding opponent from all
opponents ahead, nearby other karts from an area that may also catch the
source, a fixed track area, and non-allied karts from everyone except the
source. Documented mode availability is optional reference metadata (`None`
means unrecorded, not unrestricted) and is never represented as a target.
