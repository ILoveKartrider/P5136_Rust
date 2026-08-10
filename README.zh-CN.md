# P5136 Rust 服务器

[한국어](README.md) | [English](README.en.md) | [简体中文](README.zh-CN.md)

如果发现翻译不自然、含义错误或术语不准确，请[提交 issue](https://github.com/ILoveKartrider/P5136_Rust/issues) 或 pull request。

这是面向韩服跑跑卡丁车 P5136 客户端的独立 Rust 服务器与连接器。原 C# 项目仅作为只读协议参考，不包含在本仓库中。

本项目尚未完整复刻商业服务器的所有功能。当前目标是稳定支持朋友之间的局域网联机流程：创建或加入房间、开始比赛、行驶、查看结算与颁奖画面，然后返回房间。Lucci、奖励道具、队伍旗帜，以及部分活动、社交和商店功能尚未实现或只返回最低限度的兼容响应。

## 快速开始

使用 Rust 1.94 或更高版本构建 release：

```powershell
cargo build --release -p p5136-cli
```

Windows Server 2012 x64 兼容构建使用相同源代码，但需要单独的 legacy target 与静态 CRT。具体步骤和验证范围请参阅 [Windows Server 2012 构建说明](WINDOWS_SERVER_2012.md)。

仓库目前使用以下固定输出路径：

```text
target/p5136-finish-kart-abilities/release/p5136.exe
```

不带参数运行 `p5136.exe` 即可打开 GUI。可在右上角选择 한국어、English 或简体中文，所选语言会在下次启动时恢复。服务器和连接器位于同一个程序的两个标签页中。

1. 在“服务器”标签页中，将必填的“客户端或 Profile 路径”设为 P5136 客户端根目录，例如 `C:\Games\KartRider_5136`。
2. 如果需要让另一台电脑连接，请选择“自动设置本机局域网 IPv4”；检测到多个网卡时，请选择真实的局域网网卡。
3. 远程昵称首次连接时，请启用“允许在局域网创建新昵称”。
4. 选择“启动服务器”。
5. 在“连接器”标签页中填写游戏目录、昵称和服务器 IPv4，然后选择“准备并启动客户端”。

连接器会通过不可变的 pristine 备份和进程锁准备 PIN/XML，随后使用 Windows UAC、Wine、CrossOver 或 macOS Sikarugir wrapper 启动客户端。手动 prefix 与 wrapper 设置请参阅 [macOS Sikarugir 指南](MACOS_SIKARUGIR.md)。

## 自动发现客户端数据

可以选择客户端根目录、`Profile` 目录或 `Data` 目录。服务器会定位 `Data`，并使用有边界限制的只读解析器直接读取以下数据。无需使用 C# 生成的 `Profile/KartCatalog.xml`。

- `Data/kart.rho` 与 RHO5 overlay：车辆名称和行驶物理参数
- RHO5 `zeta_/kr/shop/data/item.kml`：库存目录
- `Data/item.rho`：各车辆道具转换，以及个人/组队概率表
- `Data/track_common.rho`：各模式赛道池和随机赛道选择器
- 其他 RHO5 数据：徽章和其他已支持目录

默认的道具概率模式是“服务器启动时自动应用”。服务器启动前会读取实际的 `item.rho`，在 GUI 中显示个人/组队条目数量与数据来源，并将该不可变快照传入本次服务器进程。加载失败时会阻止启动。只有使用“加载并固定”或 XML override 后，概率值才不再依赖客户端路径。

Sebec V1 等车辆的道具转换同样来自 `item.rho` 的 `transformByKart`，最终获得的道具由服务器决定。例如 Sebec V1 的黄金盾是服务器转换结果，并非客户端自行转换。基础与韩服 `animalBooster` 表也会单独合并，因此 Pharaoh HT、Bastet X 等车辆会获得道具 31，再由客户端显示对应车辆的黄金加速器。黄金盾与黄金加速器是两个独立效果。内存中的目录不可变，服务器不会修改 RHO 文件或客户端目录。

## 按昵称管理车辆库存

展开“按昵称编辑库存”，可以为一个昵称发放同款车辆的多个副本，并分别保存强化状态。

1. 设置服务器客户端路径和 Profile 存储路径。
2. 选择“加载车辆列表”，从客户端 RHO 读取真实名称和 ID。
3. 输入要编辑的昵称，或复制当前连接器昵称。
4. 使用名称、忽略空格后的名称或 `1410` 这样的数字 ID 搜索，然后选择结果。
5. 选择“添加所选车辆”。重复操作会分配新的唯一序列号。

默认只自动发放名称已解析、具有 `BodyParam`、存在实际 `model.1s`，并且不带测试、dummy 或 NPC 特征的车辆。在原版 P5136 数据中，1,296 辆车里有 1,287 辆通过检查。为避免滚动库存时客户端崩溃，9 个 ID（`199, 312, 323, 352, 657, 658, 659, 814, 886`）保持隔离。若确认某辆隔离车辆可以正常使用，请输入其准确数字 ID，并选择标有“需要人工确认”的结果。该操作只为对应昵称添加序列号 2 或更高的副本，不会重新启用默认序列号 1 的自动发放。

额外副本会原子写入昵称 Profile 的 `GrantedKarts`。它们与调校、工厂、等级和配件数据共用 `(kart_id, serial)` 键，因此每个副本可以保存不同强化。分配器还会避开 `TuneData.json`、`PlantData.json`、`LevelData.json` 和 `PartsData.json` 中仍在引用的序列号。若 Profile 不存在，首次发放时会自动创建。已经在线的客户端需要重新连接。

旧引擎强化采用简化的朋友服务器规则：目标车辆和材料车辆必须已拥有，但不会消耗材料或货币，成功率固定为 100%。等级、剩余点数、四槽分配和特殊效果按昵称保存到 `LevelData.json`，并恢复到库存和比赛物理中。每槽限制为 0–10，总分配上限为 35 点。

原版 floater UI 已支持插槽生成、激活套件、保护扳手和重置。消耗品不会扣除，但服务器会验证所有权、车辆类型与插槽状态，并原子保存到 `TuneData.json`。黑色套件值 `603/703/903` 分别贡献起步加速时间 `+800`、变形加速度 `+0.018` 和漂移逃逸力 `+210`。普通速度值 `103–903` 沿用 C# 数值。20 种道具 Floater 的含义均来自韩服 P5136 RHO5 的 `zeta_/kr/enchant/desc.xml`。盾牌变超级盾、水炸弹变毒性／冰冻水炸弹、导弹变黄金导弹、加速器变警笛／超级盾以及香蕉变水雷均接入服务器道具箱发放流程。每个概率均使用 `enchant.xml` 对应 Tune 的原始数值（例如 `10503` 为 15%、`11103` 为 40%、`11403` 为 25%、`11803` 为 30%）；RHO 定义的替代使用效果为 100%。与此不同，78 条 `firing2Gain` 使用触发奖励和 150 条 `fired2Gain` 受击触发奖励由客户端自行判定并发放。服务器仅按原始字节转发对应的 GameSlot 类型 10/11，不再合成重复奖励。

在 GUI 中发放车辆时，可以继续使用“333 预设”，也可以通过纵向排列的三个下拉框分别选择 Floater 槽位。编辑器提供 27 个已验证的竞速代码，并按 RHO 确认后的实际含义显示全部 20 个道具效果。若同一竞速效果类别或相同道具代码重复出现，会在创建车辆前拒绝。也可以独立勾选“应用强化 5”。

默认车辆库存现已恢复使用共享模型的 Boxter HT-S／HT-B／HT LE，以及历史内部名含 `dummyBox` 的 Kartneck／Kartneck X。剩余 9 个隔离条目分别是 4 个缺少韩服／默认 BodyParam 的车体和 5 个明确的测试或占位车体。

编辑器会短暂获取服务器使用的同一 Profile 根目录租约，并重新验证存储身份。因此，另一进程中的服务器会阻止离线编辑。在线发放通过正在运行的服务器串行 Profile 队列处理。如果加载目录后更改客户端路径，已加载目录和选择会失效；发放前还会再次检查规范化后的 `Data` 路径。

## 随机赛道

服务器以只读方式解析客户端的 RHO 1.0 `track_common.rho`。它从竞速/道具模式和选择器 `0, 1, 3–8, 23, 30, 33, 40` 的真实赛道池中抽取；当前房间赛道池未耗尽前不会重复。AI 房间优先使用 `basicAi` 赛道，必要时回退到原赛道池。

在“随机赛道设置”中加载目录后，可以通过复选框编辑每个赛道池，并使用“全选”“全部清除”和“客户端默认值”。“应用到运行中的服务器”会原子替换全部赛道池，从下一场比赛起生效，不会改变正在加载或进行中的比赛。空的自定义赛道池会在应用或启动前被拒绝。手动 override 需要已配置的客户端 `Data` 目录，以便验证 ID。

赛道与赛道池专有名称直接来自韩服客户端数据。因此，即使 GUI 界面语言设为 English 或简体中文，这些名称仍会按客户端原文显示。

## 房间标题中的 S0–S8 物理预设

房间标题中的独立 `S0`–`S8` 标记会为下一场比赛选择对应的现代 C# 物理预设：

服务器管理中的“计时赛物理预设”提供“默认设置（客户端选择）”以及 S0–S8。默认设置保持原有的客户端请求等级；选择 S 等级后，服务器重启起会覆盖每次计时赛开始响应中的 235 字节物理块。

```text
[S0] Beginner
Friendly S2
S4 Infinite Booster
```

标记不区分大小写，并使用 ASCII 字母数字边界，因此 `TESTS1ROOM` 和 `S10` 不会误匹配。修改房间标题会立即广播标题和密码，但物理预设从下一场比赛开始应用。未填写标记时，普通竞速使用 S7，道具模式使用 S8，个人/组队无限加速使用原版普通无限加速预设 S4。S6 是特殊活动预设，只有明确写入房间标题时才使用。每名玩家会收到一个 235 字节的开赛物理块，由房间默认值与该玩家的车辆、宠物和装备共同组成。

## 观察者账号

在“连接器”标签页中启用“观察者模式（pmap 718）”，即可请求观察者房主启动 Profile。服务器会原子保存该昵称的 `pmap`；关闭选项并重新连接后恢复普通值 `0`。与 C# 一样，pmap 718 会进入观察者槽位 8–15，同时保留真正的 `RoomMaster` 权限，因此普通车手加入后仍可更换赛道并开始比赛。普通观察者 pmap 590 不会自动获得这些权限。

观察者聊天使用带观察者 ID 8–15 的 `GrRiderEchoPacket`，并发送给房间内所有其他成员。Rust 回归测试会验证普通车手能够收到该 frame；若要确认原版客户端的精确显示效果，仍建议同时抓取观察者与普通账号的实时数据。

## 队伍与下一场起跑位

新车手加入组队房间时，会进入人数较少的一队；人数相同时优先蓝队，因此初始顺序为蓝、红、蓝、红。物理槽位仍为蓝队 0–3、红队 4–7，AI 也计入队伍人数。

结算后，服务器会把包括 DNF 在内的完整确认顺序保存到等待房间中每个 `RoomPlayer.ranking`。下一次 `GrCommandStart` 会序列化这些值，因此上一场结果会影响下一场起跑位。离开的车手会被删除，其余完赛顺序保持不变，并从 0 开始压缩。

回到大厅后也会重新分配房主。个人模式选择仍在房间且上一场排名最高的真人车手；组队模式选择确认获胜队伍中排名最高的剩余真人车手。AI、观察者和已离开的车手不参与选择。

## 局域网地址与域名

绑定地址决定服务器在哪个本机网卡上开放端口；公告 IPv4 会写入登录和频道数据包。自动局域网配置会排除 loopback 与 link-local 地址，优先物理网卡而非虚拟网卡，并优先使用私有 IPv4 范围。Wi-Fi、以太网、WSL、VMware、VPN、Tailscale 等网卡同时存在时，请选择与客户端处于同一网络的地址。`0.0.0.0`、组播和广播地址不能作为公告地址。

绑定地址、公告地址和连接器服务器地址目前只接受 IP literal。公告地址会按四个 IPv4 字节序列化，因此不能直接把域名写入 P5136 数据包。若必须使用域名，请在服务器启动前将它解析为固定的 A 记录地址。

GUI 会将服务器/连接器输入保存到操作系统应用存储中，包括地址、端口、路径、昵称、runner、Wine/CrossOver/Sikarugir 值、高级限制、文件日志选项、编辑后的概率表、随机赛道选择和 GUI 语言。运行状态、日志、已加载目录和临时搜索结果不会保存。

Windows 会优先加载 Malgun Gothic、Microsoft YaHei 和 SimSun；macOS 会加载 Apple SD Gothic Neo、PingFang 和 AppleGothic；Linux 会搜索 Noto Sans CJK 与 Nanum Gothic 字体族。找不到合适的 CJK 字体时会记录警告。

基准端口为 `39311` 时：

| 用途 | 协议 | 端口 |
|---|---:|---:|
| 游戏 | UDP | 39311 |
| 登录 | TCP | 39312 |
| P2P/转发 | UDP | 39312 |
| Messenger | TCP | 39313 |

从两台电脑测试时，请在服务器电脑的防火墙中允许这些 TCP/UDP 端口。

## 命令行使用

仅运行服务器：

```powershell
p5136.exe server `
  --bind 192.168.1.10 `
  --advertise 192.168.1.10 `
  --client-dir C:\Games\KartRider_5136 `
  --allow-remote-profile-creation
```

仅运行连接器：

```powershell
p5136.exe connect `
  --game-dir C:\Games\KartRider_5136 `
  --username player1 `
  --server 192.168.1.10
```

macOS Sikarugir 示例：

```bash
p5136 connect \
  --game-dir "/Users/player/Games/KartRider_5136" \
  --username player \
  --server 192.168.1.10 \
  --runner sikarugir \
  --sikarugir-app "/Users/player/Applications/Sikarugir/kartrider.app"
```

使用 `p5136.exe --help`、`p5136.exe server --help` 和 `p5136.exe connect --help` 查看全部选项。

## 日志与故障排查

每次运行都会在可执行文件旁创建新的日志：

```text
logs/p5136-<timestamp>-<pid>.log
```

GUI 会显示日志的绝对路径。默认情况下，收发数据包会写入详细文件日志。未知数据包会记录有限长度的原始字节，然后在不回复的情况下被消费，从而避免一个尚未支持的菜单终止整个登录会话。

调查客户端崩溃时，请同时保留服务器日志和客户端的 `logs` 目录。

## 测试

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

真实客户端 RHO smoke test 需要单独运行：

```powershell
$env:P5136_CLIENT_DATA_DIR='C:\Games\KartRider_5136\Data'
cargo test -p p5136-server configured_real_client_catalog_matches_the_known_p5136_shape -- --nocapture
```

协议证据、已完成范围和后续工作记录在 [PORTING.md](PORTING.md)、[PORTING_STATUS.md](PORTING_STATUS.md)、[CLIENT_PROTOCOL_FSM.md](CLIENT_PROTOCOL_FSM.md) 与 [ITEM_GAMEPLAY_COVERAGE.md](ITEM_GAMEPLAY_COVERAGE.md) 中。

## 安全与许可证

工作区强制设置 `unsafe_code = "forbid"`。所有外部文件都由带边界检查的只读解析器处理，Profile 持久化使用临时文件和原子替换。

许可证：`AFL-3.0`。
