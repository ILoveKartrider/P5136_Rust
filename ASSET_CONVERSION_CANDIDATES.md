# P5136 asset conversion candidates

Last updated: 2026-08-12

This report is the retained summary of a complete planner pass over the newer
Chinese `Data` tree and the current Korean P5136 `Data` tree. It contains only
paths and classifications; it does not contain proprietary asset payloads.

## Result

| Category | Source-only groups | Direct static candidates | Native backport required | Unresolved |
|---|---:|---:|---:|---:|
| Kart | 168 | 57 | 111 | 0 |
| Character | 73 | 73 | 0 | 0 |
| Pet resource group | 24 | 24 | 0 | 0 |
| Flying pet | 47 | 47 | 0 | 0 |
| Track-shaped folder | 53 | 34 ordinary catalog rows | 0 | 19 special/unregistered |
| Total | 365 | 235 | 111 | 19 |

`exceed` by itself is not a native-backport marker. Korean P5136 already
supports ordinary V1 Exceed and serializes its nine instant-acceleration fields
inside the stock 235-byte KartSpec snapshot. The corrected planner also
reclassified the P5136-resident `spectorV1` reference group as
`compatible_candidate`. XUN/Kart12 classes and resources remain gated.

These are static candidates, not completed client imports. Every generated
manifest still requires an isolated staging-client test covering inventory,
garage preview, loading, driving or character animation, room synchronization,
and the result screen. Global display-name/catalog rows are outside each asset
folder; their Korean/English overlay generation remains a separate localization
step even when the per-asset payload contains no Han strings.

## Kart candidates (57)

- `arowanaV1`, `artemisV1`, `blackBeatleV1`, `blitzV1`, `candy_xmasV1`, `carriageV1`, `carrotcraftV1`, `chinaV1`
- `cronosV1`, `deliveryV1`, `djV1`, `dragon_goldV1`, `dragon_redV1`, `flowerCarriageV1`, `goldKnightV1`, `houndV1`
- `hyperV1`, `lionmaskV1`, `lionmaskV1_gold`, `longlifeV1`, `magmaV1`, `mantisV1`, `marathonV1_xmas`, `mechanicdragon_blueV1`
- `mechanicdragon_redV1`, `octopusV1`, `paragonV1`, `paragonV1_gold`, `pigV1`, `roadsterV1`, `rollerBrushV1`, `rollerBrushV1_gold`
- `run_cakebok`, `run_cakechu`, `run_cakehae`, `run_cakejung`, `run_cakepi`, `run_zombie_chinese`, `saintV1`, `shefferV1`
- `Sinsu_GiV1`, `Sinsu_LinV1`, `skunaV1`, `spector_dragonV1`, `spinteacupV1`, `sprintV1`, `stalkerV1`, `SteamIV1`
- `SteamSV1`, `stingRayV1`, `stormbladeV1`, `stormbladeV1_gold`, `swordV1`, `turtleV1`, `unicorntubeV1`, `yongyong_redV1`
- `yongyongV1`

All 57 passed the current dependency and `.1s` family checks. In particular,
`arowanaV1` and `artemisV1` are no longer rejected merely because their
`BodyParam` selects ordinary `exceed`.

## Character candidates (73)

- `abysschaser`, `BaiSuzhen`, `bazzi_20year`, `bazzi_pisces`, `bazzi_shark`, `bazzi_trump`, `bongyeom`, `brodi_archer`, `caocao`, `charles`
- `chipaowolhee`, `daji`, `damarhou`, `dao_aquarius`, `dao_trump`, `deliveryman`, `dizni_aries`, `Eluna`
- `ethi_cancer`, `eunrang`, `Gstorm`, `hantao`, `James`, `kayla`, `kephi_capricorn`, `KunPengforce`
- `kwanwoo_richesgod`, `LeiZhenzi`, `Lingling`, `Lingling_baby`, `liubei`, `marid_robot`, `marid_virgo`, `mayyangyang`
- `mobi_baby`, `mos_taurus`, `mrkart`, `myeonglee`, `nezha`, `nymph_Libra`, `panda_paper`, `pengu`
- `pengzi`, `Reto_Liondance`, `Reto_Raincoat`, `rick_baby`, `run_bazzi_zombie`, `run_cakebok`, `run_cakechu`, `run_cakehae`
- `run_cakejung`, `run_cakepi`, `run_damarhou`, `run_dao_zombie`, `run_dizini_zombie`, `run_zombie_chinese`, `ShenGongbao`, `sunquan`
- `sword`, `taigong`, `tiera_paper`, `tiera_scorpio`, `tutu_baby`, `uni_Gemini`, `wonwon_baby`, `wonwon_Leo`
- `wonwon_org`, `xiaoqing`, `xiyangyang`, `yangjian`, `yongtaek`, `zhou`, `zombie_chineseFe`

The earlier `costumeSet.bml` pseudo-candidate was a grouping bug: it is a
root-level catalog file, not a character directory, and is no longer counted.

## Pet candidates (24)

- `Alpaca`, `babyPenguin`, `bazzi_block`, `bazzi_zombie`, `bluedudu`, `celestialdog`, `curseragdoll`, `dao_block`
- `dizini_block`, `dragonBoat_zongzi`, `GiLin_Gi`, `GiLin_Lin`, `jujak`, `juju`, `milu`, `moonRabbit`
- `redHorse`, `snowtiger`, `squirrel`, `weasel`, `wonwon_mermaid`, `xyy1`, `xyy2`, `yellowCow`

The source item table has 22 codes absent from P5136. `xyy1` and `xyy2` bring
the audited resource count to 24 because their source resource groups are
missing from the target even though the target already has matching catalog
codes. Multiple pet item IDs can legitimately share one model code, so catalog
merging preserves every matching row instead of treating the code as unique.

## Flying-pet candidates (47)

- `flying20year`, `flyingAnchor`, `flyingAquarius`, `flyingArcher`, `flyingAries`, `flyingBabydragon`, `flyingBambooPanda`, `flyingBambooPole`
- `flyingBlackBeatle`, `flyingBlueDragon`, `flyingBlueSnake`, `flyingCancer`, `flyingCapricorn`, `flyingCornusGreen`, `flyingCornusRed`, `flyingDragonjewel`
- `flyingFirecracker`, `flyingGemini`, `flyingGoldRing`, `flyingGourdbottle`, `flyingheartstone`, `flyingHoneyBee`, `flyingkite`, `flyingKunPeng`
- `flyingLeo`, `flyingLibra`, `flyingLonglife`, `flyingMagpie`, `flyingMechanic_blue`, `flyingMechanic_red`, `flyingMobile`, `flyingParagon`
- `flyingPisces`, `flyingQiankunRings`, `flyingRedDragon`, `flyingRedlight_Nday`, `flyingScorpio`, `flyingshadow`, `flyingSpringmine`, `flyingTaurus`
- `flyingTheArtofWar`, `flyingVirgo`, `flyingWhale`, `flyingWhiteSnake`, `flyingYinyangMirror`, `flyingZenith`, `flyingZongziPanda`

All 71 pet/flying-pet groups resolved with zero missing references and no new
native marker. Full smoke staging reopened 1,230 pet resources in three
archives and 2,277 flying-pet resources in four archives. Twenty-nine flying
pets received a guarded `param@cn.bml` to `param@kr.bml` regional alias.
Server-side physics or special effects for IDs outside P5136's existing tables
remain a separate semantic implementation boundary.

## Track-shaped resources (53; 34 ordinary candidates)

- `beach_R05`, `china_I10`, `china_R01_sn10`, `china_R10`, `jurassic_R02`, `moonhill_R06`, `village_L03_06`, `village_L03_07`
- `village_L04_06`, `village_L04_07`, `wkc_L01`, `china_I11`, `china_I12`, `china_I13`, `china_R11`, `china_R12`
- `china_R13`, `china_S02`, `fengshen_I01`, `fengshen_I02`, `fengshen_I03`, `fengshen_I04`, `fengshen_I05`, `fengshen_P01`
- `fengshen_R01`, `fengshen_R02`, `fengshen_S03`, `fengshen_S04`, `fengshen_S05`, `fengshen_S06`, `fengshen_S07`, `forest_I10`
- `gold_R03`, `ice_I09`, `ice_I10`, `ice_I11`, `ice_R07`, `ice_R08`, `ice_S02`, `ice_S03`
- `mine_I06`, `mine_I07`, `mine_I08`, `mine_I09`, `mine_R06`, `mine_R07`, `mine_R08`, `mine_S04`
- `moonhill_I06`, `northeu_I10_kd`, `pirate_R06_kd`, `village_I15`, `wkc_R12`

PNG/JPEG embedded metadata is no longer treated as a runtime dependency codec.
The previous track false positives were atlas-editor source paths retained in
thumbnail metadata, not resources consumed by the track loader.

The per-folder pass did not cover global track tables, AI paths, shared theme
materials, P5136 thumbnail aliases, or theme BGM/UI resources. A subsequent
audit found 34 active ordinary track rows, nine blocked special rows, and ten
unregistered/dormant folders. The integrated track importer applies the
additional catalog and dependency checks; do not feed all 53 folder manifests
directly to a bulk importer.

## Native-gated kart boundary

The other 111 kart groups select `xun` in their path or structured
`BodyParam`, including XUN-named resources and the less obvious `Lodi_blast`,
`Lodi_mercury`, `Lodi_swift`, and zombie-run kart groups. They remain excluded
until the XUN tachometer, charger/flat-gauge state, lead-charge transition, and
Kart12-specific native dependencies are available.

## Reproduce the reports

The planner writes both `compatibility-report.json` and
`compatibility-report.md`, plus one guarded manifest per group:

```powershell
target\release\p5136-assets.exe plan `
  --source-data C:\Nexon\launcher_v2\Data `
  --target-data C:\Nexon\KartRider_5136\Data `
  --output C:\Temp\P5136-kart-candidates `
  --category kart `
  --max-assets 1000
```

Repeat with `--category character`, `--category pet`, `--category flying_pet`,
and `--category track`. The report directory
contains an automatically invalidated `.index-cache`, so later passes avoid
reopening every legacy RHO when the source and target metadata are unchanged.
