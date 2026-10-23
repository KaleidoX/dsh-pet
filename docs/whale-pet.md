# whale-pet 技术说明

面向维护者的实现说明：工作区结构、构建、运行时机制与两个平台的窗口处理。
产品介绍与使用方式见仓库根目录的 [README](../README.md)。

## 1. 工作区结构

| 路径 | 职责 |
| --- | --- |
| `crates/pet-config` | 解析上游 `config.jsonc`：动画池与分类、`moves`、`physics`、`animationWeights`、`font`、`pets[0]` 的 `position` 与 `fixedEnabled`；画布、命中框与运动常量；素材目录解析 |
| `crates/pet-model` | 纯逻辑：随机链（`rollKind`）、分类权重、动画池、漫游规划、甩抛物理、释放速度估计、落地压缩、点击得分、拖动弹簧、Q 弹曲线；姿态描述子与切换代价（`describe_frame` / `pose_cost` / `pick_entry`） |
| `crates/pet-decode` | `ffmpeg-next` 流式解码 VP9-alpha WebM，按 PTS 交付 RGBA 帧，并上报容器时长；`starting_at` 从指定帧开始播放，`decode_all` 供离线工具整段遍历 |
| `crates/pet-ui-core` | 与窗口系统无关的行为：动画链、解码推进、弹簧拖动、甩抛、漫游、Q 弹、命中测试、点击积分、菜单动作、浮层意图、姿态匹配的关键帧硬切、重绘节奏、`--stats` 计量 |
| `crates/pet-overlay` | 交互浮层的状态与绘制：悬浮提示、积分粒子与卡片、级联右键菜单；只依赖 egui，不依赖具体窗口系统 |
| `crates/pet-ui-egui` | eframe 宿主：主窗口与浮层子视口、egui 事件循环、平台集成（X11 / Windows） |
| `crates/pet-wayland` | 原生 Wayland 宿主：主 layer surface、浮层 layer surface（`Layer::Overlay`）、`wgpu`、`egui-wgpu`、`calloop`（仅 Linux） |
| `crates/pet-win32` | Windows 全局指针读取、工作区查询与 DWM 边框关闭；唯一直接调用 Win32 的 crate |
| `apps/whale-pet` | 二进制入口：参数解析、素材定位、宿主选择 |
| `tools/anim-meta` | 离线姿态元数据生成器：解码全部 WebM，写 `assets/anim-meta.json`（不进 `default-members`） |
| `xtask` | `build-ffmpeg` / `run` / `anim-meta` / `check` 命令 |

依赖方向：`pet-config → pet-model → pet-decode → pet-ui-core → pet-overlay → pet-ui-egui / pet-wayland → apps/whale-pet`；`pet-win32` 只在 Windows 目标上被 `pet-ui-egui` 引用，`pet-wayland` 只在 Linux 目标上被 `apps/whale-pet` 引用。

workspace 级 lint 为 `unsafe_code = "forbid"`。`pet-win32` 与 `pet-wayland` 有意不继承 workspace lints：前者的
`GetCursorPos` / `GetAsyncKeyState` / `DwmSetWindowAttribute` 与后者的
`Instance::create_surface_unsafe`（wgpu 没有从裸 `wl_surface` 建表面的安全入口）都没有安全等价物。
两个 crate 各自把 `unsafe` 限制在一个模块（`pet-win32` 的 Win32 封装、`pet-wayland` 的 `src/ffi.rs`）。

## 2. 构建

### 2.1 解码库

`cargo xtask build-ffmpeg --target linux|windows` 下载固定版本、校验 SHA-256 后裁剪构建：

- FFmpeg 7.1.5、libvpx 1.15.0、nasm 2.16.03（版本与哈希写在 `xtask/src/main.rs`）
- 只启用 `libvpx_vp9` 解码器、`matroska` 解复用、`vp9` 解析、`file` 协议与 `swscale`
- 产物在 `third_party/ffmpeg/out/{linux,windows}`，每个目标有独立的源码树与安装前缀
  （Windows 安装会改写 `.pc` 并混入 `.dll.a`，与 Linux 共用目录会互相污染）
- 28 线程下约 14 秒；Linux 运行时实际加载的 5 个库 strip 后约 2.9 MB

libvpx 在 Windows 上不支持共享库，静态链入 `avcodec`。这些产物全部被 `.gitignore` 排除。

### 2.2 应用

```sh
cargo xtask run      # release 构建并启动，自动设置 FFMPEG_DIR=out/linux
cargo xtask check    # rustfmt --check + clippy -D warnings + pet-model tests
cargo xtask anim-meta   # 重新生成 dsh-pet/assets/anim-meta.json
```

`anim-meta` 默认写到素材根（`--asset-dir` 可覆盖），106 段约 13 秒，产物约 282 KB，生成结果字节确定。

`check` 不代设 `FFMPEG_DIR`，需显式提供：

```sh
FFMPEG_DIR=$PWD/third_party/ffmpeg/out/linux cargo xtask check
```

`ffmpeg-sys-next` 不追踪 `FFMPEG_DIR`：切换构建目录后要删除 `target/*/build/ffmpeg-sys-next-*` 才会重新探测。

### 2.3 Windows 交叉编译

需要 mingw-w64 工具链与 libclang（bindgen 用）。示例变量：

```sh
FFMPEG_DIR=$PWD/third_party/ffmpeg/out/windows \
LIBCLANG_PATH=<llvm>/lib \
BINDGEN_EXTRA_CLANG_ARGS="--target=x86_64-w64-mingw32 -I<mingw>/usr/x86_64-w64-mingw32/include -I<mingw>/usr/lib/gcc/x86_64-w64-mingw32/14-posix/include" \
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
cargo build --release -p whale-pet --target x86_64-pc-windows-gnu
```

`PKG_CONFIG_LIBDIR` 指向 `out/windows/lib/pkgconfig` 后 clippy 交叉检查也可用。

## 3. 配置与参数

| 参数 | 作用 |
| --- | --- |
| `--asset-dir <DIR>` | 指定素材根目录 |
| `--stats` | 每秒输出 fps、CPU、RSS、贴图占用（仅 Linux） |
| `--force-x11` | 跳过原生 Wayland，强制 X11/XWayland 后端（见 5.1） |
| `--force-wayland` | 只用原生 Wayland；初始化失败直接报错而不回退（调试用） |
| `--no-force-x11` | 兼容性 no-op：「不强制 X11」现在已是默认行为 |
| `-h, --help` | 帮助 |

环境变量：`WHALE_PET_ASSET_DIR` 指定素材根。

Wayland 会话（存在 `WAYLAND_DISPLAY`）下默认先走原生 layer shell（见 5.1），初始化失败则记 warning 并
强制 XWayland 重试——回退时会显式选中 X11，否则 winit 会再次挑中 Wayland 并以同样原因失败。

素材根解析顺序：`--asset-dir` → `WHALE_PET_ASSET_DIR` → `<exe>/assets` → 仓库内 `dsh-pet/assets`。
`config.jsonc` 读取失败时回退默认配置并告警；动画池为空时用内置默认动画名。

窗口位置取 `pets[0].position` 的 `corner`（`top-left` / `top-right` / `bottom-left` / `bottom-right`）与
`marginX` / `marginY`（逻辑像素），按主显示器的工作区计算，画布 640×360 物理像素。

配置里与上游同名、但本应用按裁决只取一部分或按固定值处理的字段：

- `pets[0].size` 解析后忽略：画布恒为 640×360 物理像素，一切行为按上游 462 基准宽度归一（`WORLD_SCALE = 640/462`），
  字号等 `* size` 表达式即退化为常数。
- `pets[0].fixedEnabled` 为真时随机链的 `turn` / `move` 权重按 0 计，只剩待机与分类动作。
- `pets[0].animationWeights` 优先于顶层 `animationWeights`（`idle` / `turn` / `move`，百分数）；
  余下权重落到按 `weight` 抽取的 `animations.categories`。
- `confineToScreen`、`physics.petCollision`、`animations.events` 只解析不使用：本应用单屏单宠，且未接入 dsh。

## 4. 运行时

### 4.1 解码与播放

每个 clip 一个解码线程，线程名 `pet-decode`。解码器固定用 `libvpx-vp9`：按 codec id 解析到的默认 `vp9`
解码器会丢掉 WebM 的 alpha 边流。帧转成紧密排布的 RGBA8，保持素材原生分辨率（640×360），不裁剪、不缩放。

解码线程按帧的 PTS 与播放时钟交付，最多提前 120 ms 排队；UI 一次只持有一帧，内存随分辨率而非时长增长。
贴图仅在 `(play_gen, seq)` 变化时重传。clip 播放结束后保留最后一帧，拖拽期间即使 clip 播完也保持悬空姿态。

播放规则与上游网页客户端一致：启动固定播 `idle[0]` 一次；待机池排除当前 clip；点击池不排除；
拖动、点击播完一遍后回待机；`turn` 播完先把 `facing` 翻转（左右镜像随之切换），再进入随机链；
分类动作与漫游各自播完再回随机链。
连续 3 个 clip 一帧都没解出时停止播放（`stalled`），避免空转。

随机链（上游 `playIdle`）：每次按 `animationWeights` 掷一次（`idle` → 待机、`turn` → 转向、`move` → 漫游，
其余 → 按 `weight` 抽分类动作）。`move` 抽中但无法起步（没有 move 动作，或落点越界）时退化为分类动作；
漫游中再次掷到 `move` 则重播当前 clip，不另起一段。

切换 clip 一律硬切，没有淡入淡出。切换时机分两类：

- **自然结束**（`Switch::Natural`，只从 `on_clip_end` 进入）：上一个 clip 已播到最后一帧，用它的末帧姿态在下一个 clip
  的**入场窗口**里挑代价最低的一帧作为起点，硬切过去；改善不足 `ENTRY_MIN_GAIN`（0.005）时仍从第 0 帧开始。
  末帧本身会先绘制一次再切换（`pending_end` 让 `on_clip_end` 推迟到下一次 `ui`），因此最后一帧不会丢；
  额外延迟是一次 ui pass（通常几毫秒，由 `hold_since` 与 8 ms 的轮询保证不会停住），不是固定的一帧时长。
- **用户主动触发**（`Switch::Immediate`：点击、开始拖动、松手、菜单动作、甩抛落地）：立即从第 0 帧硬切，不等待姿态。
- 例外：漫游（`Mode::Move`）恒从第 0 帧入场，因为行进窗口按 clip 自身时间轴（`leadSec` / `tailSec`）定义，
  从中间帧起步会让位置突跳。

切换只看姿态决定**从哪一帧进入**，不改变随机链、权重或选片结果。新 clip 的解码线程在后台启动，
第一个保留帧的 PTS 记为播放零点，因此在首帧到达前屏上继续显示旧贴图（纹理复用与 `hold_since`），
到达后同一帧上传并绘制，没有空白帧。

姿态描述子由 `cargo xtask anim-meta` 离线生成到 `assets/anim-meta.json`：

- 每帧描述为 32×18 网格（每格对应 640×360 中 20×20 像素）：alpha > 32 的轮廓掩码（576 位，hex 编码）、
  平均身体中心与头部中心（顶部 25% 高度）、脚底行、左右偏置、平均 RGB；全部整数编码，微秒时间戳，
  输出字节确定。
- 入场窗口 = 与第 0 帧代价 ≤ `ENTRY_EPS`（0.03）的最长连续前缀，上限 12 帧（`ENTRY_MAX_FRAMES`，0.5 s）。
  106 段里 80 段达到上限，最短 2 帧。
- 代价 = `0.45·(1−IoU) + 0.15·头部中心距/√2 + 0.15·身体中心距/√2 + 0.10·脚底差 + 0.05·偏置差/2 + 0.10·RGB 距/(255√3)`。
  素材统一正面站姿与脚底线，代价对「两侧同时镜像」不变，因此转向后素材空间直接比较即可。

文件缺失、版本/网格/列名不符时只记一条 warning，所有 clip 从第 0 帧进入；单个 clip 的记录不自洽
（窗口不以第 0 帧开头、窗口长于帧数、末帧时间超出 `duration_us`）时只丢弃该 clip 并 warning，其余照常。
生成器的写入路径与运行时加载器共用 `pet-config` 的同一套类型，生成后立即读回自检。每次切换打一条 debug 日志：
`cut A -> B entry=j (t ms) cost c (frame 0 c0)`，主动切换记为 `cut A -> B immediate`。

### 4.2 交互

- 命中框：画布坐标 `x 200..440`、`y 50..335`（`HIT_BOX`）
- 按下后位移达到 5 逻辑像素（`DRAG_THRESHOLD`）即进入拖动，否则松手算点击
- 点击回应带 Q 弹：0.55 倍纵向压缩，随后 ease-out-back 回弹，上限 1.12，总时长 220 ms，底边为锚点
- 光标在命中框上显示 `Grab`，拖动中显示 `Grabbing`
- 指针停在命中框上时，浮层显示宠物名字的悬浮提示；开始拖动后提示收起
- 右键命中框打开级联菜单（拖动过程中不响应）；菜单打开时左键被浮层接管，点面板外、按 Esc 或选完一项都会关闭
- `Ctrl+Q` 退出

### 4.3 拖动、甩抛与漫游

窗口位置由两轴弹簧积分：`K = 200`、`C = 30`，`physics.throwPower` 作为力的倍率；`dt` 截断到 1/30 秒。
弹簧目标 = 全局指针 − 按下时的指针偏移 − 画布中心，积分结果直接写入 `set_outer_position`。
拖动开始后停止重发启动定位（见 5.2），并把宠物按 `BOTTOM_PAD` 上抬绘制（上游「提起」姿态）。

松手时用最近 200 ms 的指针轨迹估计释放速度（`estimateReleaseVelocity`）：超过静止阈值的甩动进入弹道，
否则原地放置并提交位置。轨迹采样与上游一致，只在指针真实移动时追加一条，而不是每帧追加：拖动中窗口
跟着指针走，全局位置（`pointer_global` = 窗口原点 + surface 内位置）每帧都在变，即使指针已经停住；
每帧采样会一直刷新轨迹末端，`RELEASE_STALE_MS`（150 ms）的「松手前已停住」判定永远不成立，任何一次
拖动松手都会越过静止阈值被抛出去。X11 与 Windows 的全局指针是屏幕绝对坐标，窗口移动不改变它，这个
问题只在原生 Wayland 上暴露（实测见 7.5）。
弹道用工作区矩形做边界：左右各允许 200 画布像素的外伸（`SIDE_ALLOW`，身体贴边），
下边界为工作区底边，撞墙按 `restitution` 反弹、着地按 `groundFriction` 减速，静止后提交位置并回随机链。
着地那一帧按落地前的竖直速度触发压缩（`landingSquash`，越快压得越深）。

自主漫游按 `animations.moves` 选一个动作，用其 `minDist` / `maxDist` / `margin` 规划落点（朝当前面向方向走），
落点越界就不发车；移动在 clip 的 `leadSec` / `tailSec` 之间线性完成，走完提交位置。物理常量全部按
`WORLD_SCALE` 换算到画布像素空间。

### 4.4 交互浮层

主窗口恒为 640×360 物理像素，菜单、提示和积分卡片放不下，因此单独开一个 760×480 逻辑点的透明置顶浮层窗口，
每帧跟随主窗口位置（`overlay_origin`：以宠物为中心，向上、向左各扩出 `(760·ppp−640)/2` 与 `(480·ppp−360)/2`）。

- **三种模式**：`Hidden`（什么都不显示，浮层窗口隐藏或提交一帧空白）、`Transient`（提示/积分，整窗点击穿透）、
  `Menu`（菜单打开，整窗接收指针）。模式由 `OverlayState::tick` 在宠物的 pass 里推导，而不是在浮层的绘制回调里——
  eframe 对隐藏的子视口根本不跑 egui pass，在那里推导会永远停留在 `Hidden`。
- **坐标约定**：提示、菜单和积分卡片的锚点都是物理屏幕像素（与上游 `position:fixed` 一致）；
  浮层窗口的真实位置由宿主每帧上报（窗口管理器可能因贴边而挪动它），绘制时按该原点换算成浮层逻辑点。
  指针优先使用浮层自己收到的位置；Wayland 上浮层没有指针（穿透时不产生事件）时用宿主给出的屏幕坐标。
- **悬浮提示**：白底圆角、`#1f1f1f` 文字、单行省略；默认在指针右下，越界翻到左上，最后夹进浮层窗口。
  字号取自 `font.size`（`Font::tooltip_size`）。
- **点击积分**：按下时若宠物正在飞行且速度超过阈值，得分为速度（462 基准）/100，弹出 20 个粒子和一张 `+N` 卡片
  （标题 22 px 橙色、副行 `速度 N · 大小 462`），2.2 秒后消失；这一次按下不再播普通点击回应。
- **右键菜单**：一级「回到初始位置」+「动作」；「动作」下是待机 / 转向 / 拖拽 / 点击回应 / 移动与非空分类，
  再下一级是各池的 clip 名。面板宽 150–240、圆角 8、悬停高亮、可滚轮滚动；子面板贴所点条目右侧，
  越右翻左、越下翻上，并夹在浮层窗口与工作区的交集内。悬停分支立即展开下一级；指针离开整棵菜单 200 ms 后关闭。
  菜单期间浮层整窗接收指针，因此点面板外就是关闭。

菜单选中的动作回传给 `PetCore`：`PlayOnce` 播一次后回随机链（`noMirror` 的文字类动作朝右时先转向左），
`MoveTo` 走对应漫游（无法起步时退化为播放一次），`Home` 停止甩抛/漫游并把位置还原到配置的角落。

### 4.5 重绘调度

按需重绘：解码下一帧到期时间；拖动中每帧重绘；甩抛、漫游与 Q 弹期间每 8 ms 一次；
按住未拖动时每 8 ms 轮询一次以便窗口外的释放也能被处理；浮层由宿主在可见时每帧重绘并跟随宠物；
Windows 额外固定每 16 ms 轮询全局指针（见 5.3）。

## 5. 平台实现

### 5.1 Linux：原生 Wayland（layer shell）

`crates/pet-wayland` 用 `zwlr_layer_shell_v1` 建窗口。这条路径同时解决了 X11 节里那三个硬需求，且不依赖
XWayland：位置由 anchor + margin 决定，层级天然在普通窗口之上，`wl_surface.set_input_region()` 提供按区域穿透。

- **坐标**：交换链固定 640×360 物理像素；surface 尺寸是输出逻辑像素 `round(640/ppp) × round(360/ppp)`
  （1.5 倍下为 427×240），由 `wp_viewport.set_destination` 把缓冲拉伸到该矩形，`buffer_scale` 保持 1。
  egui 以这个逻辑矩形为 `screen_rect`、`pixels_per_point = ppp`，因此 `PetCore` 在两条路径上看到相同的输入形状。
  `FrameInput.global` / `window_origin` / `FrameOutput.move_to` 用画布像素，宿主负责与逻辑坐标互换。
- **缩放**：`wp_fractional_scale_manager_v1` 的 `preferred_scale`（KWin 给 180）除以 120 得到 `ppp`；
  `set_size` + `set_destination` + `set_input_region` 一起重新下发。没有该全局时退回 `wl_output.scale_factor`，
  KWin 只报整数 2，画布会偏大但可用。整数缩放事件不会覆盖已经收到的分数缩放值。
- **定位**：`corner` 直接映射成 `Anchor`，`marginX` / `marginY` 本来就是逻辑像素，原样作为 margin。
  拖动时反向算：`move_to / ppp` 减去输出的 `logical_position` 得到输出内逻辑坐标，再按当前 anchor 换成两个 margin；
  每帧最多一次，且被 clamp 在输出矩形内，避免弹簧追一个合成器拒绝执行的位置。
- **输出**：以 `wl_surface.enter` 为准——它指出合成器实际把 surface 放在哪个输出上，其 `logical_position` /
  `logical_size` 供全局坐标换算与拖动 clamp 使用。只有在该事件始终没来时，才退回「第一个报告逻辑尺寸的输出」。
- **穿透**：`set_input_region` 限制为 `HIT_BOX`（按 `ppp` 换算），命中框外的点击落到下层窗口。
- **拖动**：不锁指针。窗口跟随指针时指针仍在 surface 内，`wl_pointer` 的隐式抓取足够；全局位置只在收到指针事件时
  按当时的窗口原点换算并缓存——Wayland 不会因为窗口自己移动而补发 motion，每帧重算会形成反馈。释放速度的轨迹
  采样同样只认「指针真的动了」，理由与实测见 4.3 与 7.5。
- **键盘**：`KeyboardInteractivity::OnDemand`；KWin 会在指针进入后给焦点（`wl_keyboard.enter` 实测到达）。
  `Ctrl+Q` 由 `wl_keyboard.press_key` 的 keysym 直接判定，不依赖 egui 的键映射。
- **渲染**：wgpu 30.0.1 的 Vulkan 后端；优先 `Bgra8Unorm` + `PreMultiplied` alpha。`Queue::present` 在 Wayland 上
  attach + commit 一次完成，所以 viewport destination、margin、input region 都必须在 present 之前设置。
- **重绘**：`FullOutput.viewport_output[ROOT].repaint_delay` 决定下一次唤醒，`Duration::MAX` 时用 1 秒心跳兜底。
- **交互浮层**：同一连接上再建一个 `Layer::Overlay` 的 layer surface（`OVERLAY_W × OVERLAY_H` 逻辑点，
  `set_exclusive_zone(-1)`），共享 `Wgpu` 的 device/queue（`Wgpu::sibling`），拥有独立的 `egui::Context` 与 `Wgpu`。
  位置固定 `Anchor::TOP | Anchor::LEFT`，每帧按主窗口位置换算成 margin，与帧一起提交。输入区随模式切换：
  `Hidden` / `Transient` 传空 region（点击穿透），`Menu` 传整窗；空 region 必须显式创建，`None` 表示无限输入区。
  它在合成器报告出宠物所在输出之后才创建；输出变化时（`belongs_to` 不匹配）销毁重建。创建失败只降级为
  「没有浮层」，宠物本身继续工作。
- **浮层键盘**：浮层用 `KeyboardInteractivity::None`，不抢焦点；Esc 由宠物的 `wl_keyboard` 收到后转发给浮层的
  输入队列，因此 `Ctrl+Q` 也不会被浮层截走。
- **指针归属**：一个 `wl_pointer` 报告它进入的每个 surface，`pointer_frame` 按 `event.surface` 把同一帧拆给宠物和
  浮层；浮层的位置是相对它自己的 surface 的。浮层移动时不补发 motion，所以宿主在移动 surface 时对缓存的位置做
  反向补偿；输入区从空变为整窗后合成器也不立刻补发 `enter`，切换时清空缓存，改用宿主给出的屏幕坐标定位首帧。

### 5.2 Linux / X11

设置了 `DISPLAY` 且未传 `--force-x11` 时使用 X11（自动回退或显式指定）：KWin 的原生 Wayland toplevel 无法自我定位、
置顶或设置输入区域。

- 输入区域：用 X11 SHAPE 扩展把窗口输入区限制为命中框，透明区域天然点击穿透
- 全局指针：直接 `query_pointer` 读根窗口坐标与按键，因为指针一旦拖出输入区，窗口就收不到后续移动与释放事件
- 工作区：读 `_NET_WORKAREA` 的当前桌面项（多屏时为各屏并集），与窗口所在显示器矩形取交集；取不到时退回显示器矩形
- 启动定位：KWin 会忽略窗口映射前发出的移动请求，所以启动后 2 秒内每 500 ms 重发一次位置与置顶，
  用户开始拖动后停止重发
- 交互浮层：eframe 的 deferred viewport（`ViewportId::from_hash_of("whale-pet-overlay")`）。窗口每次宠物的 pass
  都要重新 `show_viewport_deferred`（egui 会回收上一帧没用到的子视口），builder 每帧整段覆盖：位置 =
  主窗口位置 − 浮层偏移，尺寸 760×480 逻辑点，`Transient` 时 `MousePassthrough(true)`，`Hidden` 时不可见。
  子窗口首次显示后要再发一次 `WindowLevel::AlwaysOnTop`（X11 的 winit 只在创建时应用层级，而 eframe 建子窗口时
  是隐藏的），并 `request_repaint_of` 唤醒它——子视口只在它自己请求重绘或收到输入时才运行 pass。

### 5.3 Windows

透明：per-pixel alpha 要求 DirectComposition 交换链（wgpu 的 `Dx12SwapchainKind::DxgiFromVisual`），
且窗口必须以 `WS_EX_NOREDIRECTIONBITMAP` 创建。`CreateSwapChainForHwnd` 路径在 wgpu-hal 30.0.1 中只报告
`CompositeAlphaMode::Opaque`；winit 又只在创建窗口时接受该扩展样式，所以由 vendored egui-winit 在建窗前设置。

穿透：`WS_EX_TRANSPARENT | WS_EX_LAYERED` 是整窗语义，没有按区域生效的顶层窗口输入形状；`WM_NCHITTEST`
返回 `HTTRANSPARENT` 只对同线程窗口生效，无法把点击交给其他进程。因此按全局光标位置逐帧切换
`ViewportCommand::MousePassthrough`：指针在命中框外且没有进行中的按下时穿透，按下期间保持可交互
（winit 在按钮按下时建立鼠标捕获，穿透窗口收不到释放）。

点击竞态：`egui-winit::on_mouse_button_input` 只在 `pointer_pos_in_points.is_some()` 时推送 `PointerButton`，
而该位置只由 `CursorMoved` 写入；窗口处于穿透时收不到任何指针事件，指针快速进入命中框后按下会被丢弃。
`fold_global_pointer` 用全局光标读数为本帧补齐位置，并用 `GetAsyncKeyState` 的下降沿补一次按下。

灰边：无边框窗口仍会被 DWM 绘制约 2 px 的非客户区边框；`DWMWA_NCRENDERING_POLICY = DWMNCRP_DISABLED`
让 DWM 完全跳过非客户区渲染。`DWMWA_BORDER_COLOR` 只能改颜色，去不掉边框。

工作区：`GetMonitorInfoW.rcWork`，与 X11 的 `_NET_WORKAREA` 语义一致；取不到时退回显示器矩形。

浮层：与主窗口一样走 eframe 子视口，子窗口继承共享 `WgpuSetup` 的 DX12 交换链与无重定向位图补丁
（`egui-winit` 在建窗时按 `window_attributes.transparent()` 判定）。模式为 `Transient` 时整窗
`MousePassthrough`；菜单打开时整窗可交互。

### 5.4 vendored egui-winit

`third_party/egui-winit/` 是 egui-winit 0.37.0 的原始副本，通过根 `Cargo.toml` 的 `[patch.crates-io]` 覆盖。
唯一改动是新增 `set_transparent_no_redirection_bitmap`。`.cargo_vcs_info.json` 记录上游提交
`7c0b40402271e8d40cda69f87cf3c2617c2e4382`，便于比对。

## 6. 已知限制

- 原生 Wayland 的热插拔与跨屏：layer surface 绑定单个输出，输出消失时停在最后已知几何，不会重建 surface
  （交互浮层在宠物换到另一个输出时会重建，但输出消失本身仍按最后几何处理）
- 原生 Wayland 启动后极短时间内若 `wl_surface.enter` 还没到，会先按「第一个报告逻辑尺寸的输出」建一次浮层；
  `enter` 到达后按 `belongs_to` 立即重建（见 7.4 的 51 ms 实测）。浮层在此期间是隐藏的，不影响交互，只是多一次创建
- 原生 Wayland 的拖动起点必须在命中框内：输入区外的点击不产生按下事件（这正是穿透的实现方式）
- 原生 Wayland 需要合成器提供 `zwlr_layer_shell_v1`；不提供时自动回退 XWayland
- KWin 在浮层输入区变化后不会立刻重新选择 surface：`set_input_region` 从空变为整窗（或反向）之后，
  `wl_pointer.enter` 要等下一次真实指针移动才到，期间浮层收不到 hover。菜单打开的首帧用宠物侧报告的屏幕坐标
  做命中测试（右键位置，且宠物此时已停止移动），要等一次真实移动之后才由 `wl_pointer` 自己的位置接管
- 浮层固定 760×480 逻辑点：三级菜单在贴边时会夹在浮层窗口内，超长列表靠滚动查看
- 指纹/触摸、输入法未接入；只处理鼠标左键/右键与键盘 `Ctrl+Q`（菜单打开时额外处理 Esc）
- `--stats` 仅 Linux 实现
- 姿态匹配只决定从哪一帧进入：没有过渡动作或形变，切换代价高的组合仍是硬切。自然结束的切换会多等一次
  ui pass，因为末帧要先绘制一次
- `anim-meta.json` 必须与素材一同发布；缺少它时所有切换都从第 0 帧开始（仍有硬切，没有淡入）。素材被换成更短的
  版本而元数据未重生成时，若选中的入场帧超出新片长，该 clip 会打 warning 并退回第 0 帧重播，不会计入解码失败
- 未验证：Windows 宿主上的实跑、小时级连续播放（解码线程与内存的长期行为），以及「素材替换但元数据未重生成」
  的现实场景（只验证了截短片长的构造场景与单 clip 校验）
- 没有安装包与发布流程，只能从源码构建
- 只解码视频轨，没有音频
- 未接入 dsh：余额、碎碎念、对话、系统通知、工作状态联动、HTTP 端点、占用锁、斜杠命令、设置页、托盘
  以及菜单里的 open-site / reload / set-display 都不存在；`animations.events` 只解析不使用

## 7. 验证记录

原生 Wayland（KDE Wayland 6.3.6，DP-2 缩放 1.5，debug 构建）：日志确认 `fractional scale 1.5` →
`427x240 logical`、`surface on DP-2 at (1440, 560)`；协议日志确认 `set_size(427, 240)`、
`wp_viewport.set_destination(427, 240)`、`set_input_region` 为命中框 `(133, 33, 160, 190)`（逻辑像素 = 物理 / 1.5）、
`preferred_scale(180)`，以及 `OnDemand` 下 `wl_keyboard.enter` 到达。系统截图显示宠物在配置的右上角、
背景透出下层窗口、无窗口装饰。稳定 24 fps（与素材帧率一致）、CPU 3–4%、RSS 约 163 MB、贴图 0.88 MB。
回退链实测：`WAYLAND_DISPLAY` 指向不存在的 socket 时记 `native Wayland failed ... falling back to XWayland`
并成功落到 X11（640×360 物理像素、scale 1.5）；`--force-wayland` 在同条件下直接报错退出（exit 1）。
本机没有 `xdotool` / `ydotool` / `wtype`，`/dev/uinput` 仅 root 可写；桌面解锁后的指针交互验证见 7.4。

X11（KDE Wayland + XWayland，DP-2 缩放 1.5，release）：稳定 24 fps（与素材帧率一致）、CPU 5–6%、
RSS 约 154 MB、贴图 0.88 MB；命中框内外的点击分别由宠物与下层探针窗口收到；Q 弹测量 271 → 148 → 279 px；
拖动窗口跟随指针，松手后保留位置。

Windows（QEMU/KVM，Windows 11 22631，虚拟机内为 WARP 软件光栅）：per-pixel 透明、四个方位的点击穿透、
50/100/200/400 ms 快速点击、拖动均通过；工作集 28.3 MB、峰值 65.6 MB、私有提交 297.8 MB、13 线程、单核约 38%。
软件光栅下的 CPU 与内存不代表有 GPU 的真实机器。

### 7.1 宠物侧交互（X11）

同一台 X11 会话下人工驱动（XTest 注入）：指针进入命中框后浮层输出
`Tooltip { anchor: (5705, 1185), text: "蓝毛小女仆" }`，提示框贴合指针右下；右键后日志出现
`context menu at (5644, 1170)` 与 `overlay intent Menu { anchor: (5644, 1170) }`；点「回到初始位置」后
`overlay action Home`，宠物回到配置的右上角。浮层子窗口为 1140×720 物理像素（760×480 逻辑点 × 1.5），
透明、无装饰、置顶，中文与菜单交互正常。菜单的级联、叶子点选、点面板外关闭与 Esc 关闭，
改在 7.2 的嵌套 Wayland 环境里逐项核对。

### 7.2 原生 Wayland 浮层（嵌套 KWin）

真机 Wayland 会话当时处于锁屏 + DPMS 关闭状态（合成器不消费帧，宠物只跑完首帧），因此浮层交互改在嵌套
`kwin_wayland`（1280×800，ppp = 1）里验证，日志与截图为该环境下的实测：

- 右键：`context menu at (936.0, 280.0)` → 浮层 `mode=Menu`，根面板 `[416,240]-[566,309]` 正确绘制
  （截图确认「回到初始位置 / 动作」与悬停高亮）。
- 级联：悬停「动作」后 `panels=2`，右面板 `[565,171]-[715,476]`；再悬停一级 `panels=3`，左翻面板
  `[384,16]-[566,476]`（越右翻左、越下翻上均命中）。
- 点选叶子：`-> Action 悠闲哼歌`，菜单关闭，浮层回到 `Hidden`。
- `Home`、点面板外（`Press` 落在面板外，下一帧关闭）、Esc（`press_key keysym=XK_Escape`，从宠物键盘转发）
  均能关闭菜单。
- 浮层 surface：`(760, 480)` 逻辑点，ppp = 1 时缓冲也是 760×480；ppp = 1.5 的真机首帧为 1140×720 缓冲。

过程中确认了两条合成器行为并据此修正代码（见 5.1 与第 6 节）：`set_input_region` 变化后需下一次真实 motion
才重新选择 surface；surface 自身移动不补发 motion，需对缓存指针做反向补偿。

### 7.3 纯逻辑探针

无窗口探针（`pet-config` + `pet-model`，不提交）逐项断言：随机链边界（`idle`/`turn`/`move` 的权重切点）、
`fixedEnabled` 下 `turn`/`move` 权重归零、抛掷越界反弹并在 30 秒内静止于边界内、撞墙反转 `vx`、
快速甩动产生抛掷而静止/陈旧轨迹退回轻放、漫游落点在 1280×800 工作区内且右边缘朝左仍可发车、
`click_score(800 逻辑 px/s) = 8`、`landing_squash` 随冲击速度单调变浅。全部通过（`ALL CHECKS PASSED`）。

`cargo xtask check`（rustfmt + clippy `-D warnings`）通过。Windows 交叉检查只覆盖不依赖 FFmpeg 的 crate
（`pet-config` / `pet-model` / `pet-win32`）；完整的 `x86_64-pc-windows-gnu` 构建需要本机没有的 mingw-w64 工具链。

### 7.4 桌面解锁后的人工指针交互（原生 Wayland）

桌面解锁（`LockedHint=no`）后用真实鼠标操作，`RUST_LOG=...=debug` 日志为证。布局为两屏：DP-1
2160×3840 竖置、逻辑 1440×2560（0, 0）；DP-2 3840×2160、逻辑 2560×1440、位于 (1440, 560)；两屏 `ppp = 1.5`。
宠物按 `top-right` + `margin 24/100` 落在 DP-2 上，画布物理左上角为 (5323, 990)，命中框因此是
x 5623–5983、y 1065–1492（物理像素）。

- 右键 10 次全部打开菜单，锚点如 `context menu at (5655.74, 1221.46)`、`(5446.41, 1283.19)`，
  全部落在上述命中框内，说明 ppp = 1.5 下 `pointer_global` 的窗口原点换算正确。
- 拖动：`-> Drag 被鼠标拖拽悬空反馈` → `drag released`；随后菜单锚点变成 `(5687.81, 1240.25)`，
  宠物已被拖到新位置，且锚点跟随新位置，说明拖动提交与浮层原点每帧跟随都生效。
- 漫游：`walk 螃蟹走路 finished at 5168`、`walk 原地左转奔跑 finished at 5094` 等 6 次，落在 1440 宽的工作区内。
- 转向：`turn ended; facing Right` 2 次，翻转后随机链继续。
- 浮层在 `(760, 480)` 逻辑点 / 1140×720 物理像素上创建，DP-2 上重建后 `overlay configure` 通过。

启动时序上有一条已知告警：`the compositor never reported the surface's output; assuming DP-1 ...`。实测它出现在
首帧之前——`wl_surface.enter` 在 51 ms 后到达（`surface on DP-2 at (1440, 560), (2560, 1440) logical, scale 1.5`），
浮层随即按 `belongs_to` 从 DP-1 上的实例重建到 DP-2。这 51 ms 内浮层一直是 `Hidden`（只提交空白帧），
拖拽也不可能在那之前开始，因此不影响交互；只有首帧可能多一次无谓的浮层创建。

本机没有 `xdotool` / `ydotool` / `wtype` 一类注入工具，`/dev/uinput` 仅 root 可写，KWin 也未暴露虚拟指针协议；
KDE 的 `org.freedesktop.portal.RemoteDesktop` 能建立会话与发送 `NotifyPointerMotion`，但绝对坐标
（`NotifyPointerMotionAbsolute`）一律返回 `Invalid position`，`ConnectToEIS` 又因本机只有服务端库 `libeis.so.1`
而无 libei 客户端库不可用。因此上述交互全部是人工操作，未做脚本化注入。

### 7.5 松手必抛（原生 Wayland）的根因与修复

现象：真机上手拖动宠物，无论多慢、松手前停多久，一松手都被甩出去。

根因在 `sample_trail`：它原先每帧调用，而上游只在 `pointermove` 里采样。Wayland 的 `wl_pointer` 位置是
surface 局部坐标，窗口跟随指针移动时不补发 motion，于是拖动中 `local` 基本恒定、窗口原点每帧在变，
`pointer_global = window_origin + local` 每帧都在漂。每帧追加一条「看似在动」的样本后，轨迹末端始终新鲜，
`RELEASE_STALE_MS` 的静止判定失效，估算出的速度必然超过 `DEAD_ZONE_SPEED`。X11 / Windows 的全局指针是
`query_pointer` / `GetCursorPos` 的真实屏幕坐标，窗口移动不改变它，因此只有原生 Wayland 出现该缺陷。

修复：`sample_trail` 只在全局位置与上一条样本不同（容差 0.01 px）时才追加，`Press` 新增 `last_sample`
字段记录上一点。这样「指针停住 → 轨迹变陈旧 → 温柔放下」在上游与两个宿主上语义一致。

嵌套 KWin（1280×800，ppp = 1）里验证：用 XTest 向外层 X 服务器注入指针事件，嵌套 KWin 作为 X11 客户端
接收后再转发给它自己的原生 Wayland 客户端。debug 日志为证：

- 修复前：慢拖（30 步 × 30 ms，松手前停 600–700 ms）→ `release vx=-1507 vy=-670 |v|=1649` 并落地弹跳。
- 修复前的事件探针：同一次拖动中 `origin` 从 `(527, 100)` 漂到 `(436, 54)`，`local` 稳定在 `(376, 175)`，
  `global` 随之从 936 漂到 813——正是「窗口移动被当成指针移动」。
- 修复后（清理掉全部探针的最终构建）：同样的慢拖 → 只有 `drag released` 与 `-> Idle`，没有 `release vx`。
- 修复后快甩（8 步 × 20 ms 的斜向甩动）→ `release vx=1564 vy=-1794 |v|=2380`、`landing squash 0.66`。
- X11 对照：慢拖停住不抛；斜向快甩 `release vx=1310 vy=-437 |v|=1380`。

### 7.6 姿态匹配硬切

生成器（`cargo xtask anim-meta`，release，106 段）：13.4 秒完成，写出 282 092 字节；两次运行的 SHA-256 一致。
统计与计划标定吻合：入场窗口 80/106 段达到 12 帧上限、最短 2 帧、中位数 12；自然结束代价（A 末帧 → B 第 0 帧）
p50 0.029 / p90 0.063 / p99 0.157 / max 0.204；11 130 个有序切换（不含 A→A）中 1 931 个（17.3%）跳过第 0 帧，
跳帧时平均收益 0.008（p90 0.013）。写入后用运行时加载器读回自检通过。

`Player::starting_at` 探针（106 段 × 跳过 0/1/7/11 帧，共 405 次）：首个交付帧与 `decode_all` 同索引帧**逐字节相同**；
交付的 PTS 与 `anim-meta.json` 的 `entry[j].t` 一致；`duration()` 与容器时长一致；首帧最大延迟 12.8 ms（帧间隔 41.7 ms）。

无窗口 `PetCore` 探针（headless egui pass）观察 9 次自然切换与 1 次主动切换：

- 每次自然切换前最后一个上传的贴图都是上一段的**解码末帧**（按索引比对，非近似），`pending_end` 的延迟生效；
- `是啊，吃什么 -> 余额-数金皱眉` 打出 `entry=8 (333 ms) cost 0.017 (frame 0 0.039)`，而新 clip 第一个上传的贴图
  正是解码第 8 帧——选帧结果确实落到了解码器；
- `overlay_action` 在 clip 播放中触发，7 ms 内上传并绘制目标 clip 第 0 帧。

真机 X11（`--force-x11`）与原生 Wayland 各跑一次，`RUST_LOG=pet_ui_core=debug`：两条路径都观测到
`cut A -> B entry=8 (333 ms) cost 0.017 (frame 0 0.039)`，随后 `produced its first frame after 6–30 ms`（无空白帧）；
把待机池限制到高收益组合后，X11 与 Wayland 各自然切换 11 次，其中 3 次跳帧，跳帧率与生成器统计同量级。
主动切换（菜单 `PlayOnce`）打出 `cut A -> B immediate` 并立即换图。

降级：临时移走 `anim-meta.json` 后启动，只出现一条
`anim-meta.json: No such file or directory ...; animations enter at frame 0`，之后每次切换都是
`cut A -> B at frame 0`，硬切照常、无空白帧。把一个 clip 的 `entry[0].t` 改成非零后启动，只丢弃该 clip
（`dropping clip "是啊，吃什么": entry window starts at 41700 us, not frame 0`），其余 clip 的姿态匹配照常。

元数据漂移（无窗口 `PetCore` 探针，临时素材目录）：把 `余额-数金皱眉` 截成 3 帧而元数据仍是 241 帧，自然切换选中
`entry=8` 后解码线程零帧交付，打出 `delivered no frame from entry 8; replaying from frame 0` 并以 `immediate`
重播，25 ms 后首帧到达，未计入连续解码失败。

`cargo xtask check`（rustfmt + clippy `-D warnings`，覆盖 `tools/anim-meta`，并跑 `pet-model` 的 4 项 lib 测试）通过。
