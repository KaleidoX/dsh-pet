# whale-pet 技术说明

面向维护者的实现说明：工作区结构、构建、运行时机制与各平台的窗口处理。
产品介绍与使用方式见仓库根目录的 [README](../README.md)。

## 1. 工作区结构

| 路径 | 职责 |
| --- | --- |
| `crates/pet-config` | 解析上游 `config.jsonc`：动画池与分类、`moves`、`physics`、`animationWeights`、`font`、`pets[0]` 的 `position` 与 `fixedEnabled`；画布、命中框与运动常量；素材目录解析 |
| `crates/pet-model` | 纯逻辑：随机链（`rollKind`）、分类权重、动画池、漫游规划、甩抛物理、释放速度估计、落地压缩、点击得分、拖动弹簧、Q 弹曲线；姿态描述子与切换代价（`describe_frame` / `pose_cost` / `pick_entry`） |
| `crates/pet-decode` | 按 PTS 交付 WebM RGBA 帧或 macOS 原生视频缓冲区，并上报容器时长；`starting_at` 从指定帧开始播放，`decode_all` 供离线工具整段遍历 |
| `crates/pet-ui-core` | 与窗口系统无关的行为：动画链、解码推进、弹簧拖动、甩抛、漫游、Q 弹、命中测试、点击积分、菜单动作、浮层意图、姿态匹配的关键帧硬切、重绘节奏、`--stats` 计量 |
| `crates/pet-overlay` | 共享单窗口场景、画布布局与交互控件：悬浮提示、积分粒子与卡片、级联右键菜单；不依赖具体窗口系统 |
| `crates/pet-ui-egui` | eframe 宿主：宠物与控件共用一个主窗口、egui 事件循环、平台集成（X11 / Windows / macOS） |
| `crates/pet-wayland` | 原生 Wayland 宿主：宠物与控件共用一个 layer surface、`wgpu`、`egui-wgpu`、`calloop`（仅 Linux） |
| `crates/pet-macos` | VideoToolbox 强制硬解、CVPixelBuffer 生命周期、同设备 Metal 纹理导入与 GPU alpha 转换（仅 macOS） |
| `crates/pet-win32` | Windows 全局指针读取、工作区查询与 DWM 边框关闭；唯一直接调用 Win32 的 crate |
| `apps/whale-pet` | 二进制入口：参数解析、素材定位、宿主选择 |
| `tools/anim-meta` | 离线姿态元数据生成器：解码全部 WebM，写 `assets/anim-meta.json`（不进 `default-members`） |
| `xtask` | `build-ffmpeg` / `run` / `anim-meta` / `check` 命令 |

依赖方向：`pet-config → pet-model → pet-decode → pet-ui-core → pet-overlay → pet-ui-egui / pet-wayland → apps/whale-pet`；`pet-win32` 只在 Windows 目标上被 `pet-ui-egui` 引用，`pet-wayland` 只在 Linux 目标上被 `apps/whale-pet` 引用。

workspace 级 lint 为 `unsafe_code = "forbid"`。`pet-win32`、`pet-wayland` 与 `pet-macos` 有意不继承 workspace lints：前者的
`GetCursorPos` / `GetAsyncKeyState` / `DwmSetWindowAttribute` 与后者的
`Instance::create_surface_unsafe`（wgpu 没有从裸 `wl_surface` 建表面的安全入口）都没有安全等价物。
平台 crate 把 `unsafe` 限制在 FFI 边界；`pet-macos` 另外封装同一 Metal 设备上的原生纹理导入。

## 2. 构建

### 2.1 解码库

`cargo xtask build-ffmpeg [--target linux|windows|macos]` 下载固定版本、校验 SHA-256 后裁剪构建；省略目标时选择宿主系统：

- FFmpeg 7.1.5、libvpx 1.15.0、nasm 2.16.03（版本与哈希写在 `xtask/src/main.rs`）
- 只启用 `libvpx_vp9` 解码器、`matroska` 解复用、`vp9` 解析、`file` 协议与 `swscale`
- 产物在 `third_party/ffmpeg/out/{linux,windows,macos}`，每个目标有独立的 libvpx 源码树、构建目录与安装前缀
  （Windows 安装会改写 `.pc` 并混入 `.dll.a`，与 Linux 共用目录会互相污染）
- 28 线程下约 14 秒；Linux 运行时实际加载的 5 个库 strip 后约 2.9 MB

libvpx 在 Windows 上不支持共享库，静态链入 `avcodec`；macOS 也使用静态 libvpx，避免额外的运行时搜索路径。
macOS 的 FFmpeg 动态库使用仓库内安装前缀的绝对路径，本机构建后可以直接运行，移动仓库后需要重新构建解码库及应用。
`cargo xtask package-macos` 会将实际依赖的动态库复制进应用的 `Contents/Frameworks` 并改写为包内相对引用，
把素材放进 `Contents/Resources/assets`，生成可搬迁的 `.app` 和 DMG。包内含安装说明、项目与第三方许可、解码库源码；
只构建当前二进制的架构，版本取自 workspace。产物在 `release/v<版本>/`，附独立 `.sha256`，默认使用 ad-hoc 签名。
Apple Silicon 使用原生 ARM 汇编，不构建 NASM；Intel macOS 使用 NASM。需要 Xcode Command Line Tools、`make`、`pkg-config`、`curl` 和 `tar`。
这些产物全部被 `.gitignore` 排除。

### 2.2 应用

```sh
cargo xtask run      # release 构建并启动，自动设置宿主平台的 FFMPEG_DIR
cargo xtask run --debug -- --asset-dir /path/to/assets  # 开发构建，转发应用参数
cargo xtask check    # rustfmt --check + clippy -D warnings + workspace tests
cargo xtask anim-meta   # 重新生成 dsh-pet/assets/anim-meta.json
```

`anim-meta` 默认写到素材根（`--asset-dir` 可覆盖），产物约 282 KB。生成器按片名排序，固定输出布局；
跨架构的解码/缩放实现可能使 RGB 描述子存在微小差异。macOS arm64 实测完整解码 106 段约 17 秒，
生成结果与仓库元数据的帧数、时长、轮廓和姿态字段一致，仅 RGB 描述子不同。

`run`、`check` 和 `anim-meta` 共用宿主解码库目录，也接受显式的 `FFMPEG_DIR`：

```sh
FFMPEG_DIR=/path/to/ffmpeg cargo xtask check
```

`ffmpeg-sys-next` 不追踪 `FFMPEG_DIR`：切换构建目录后要删除 `target/*/build/ffmpeg-sys-next-*` 才会重新探测。

### macOS 原生视频链路

`cargo xtask fetch-macos-assets` 获取上游 [assets-mov](https://github.com/PC2005-cloud/dsh-pet/releases/tag/assets-mov) 发布的 106 段 HEVC-alpha MOV，缓存在 `.tools/macos-assets/mov/`。固定 ZIP 校验和在 `scripts/fetch_macos_assets.py`；下载标签被覆盖且哈希改变时停止，不静默接受新素材。每次准备同时核对片名、ZIP CRC 和缓存内容。

`cargo xtask run` 在 macOS 上自动准备缓存并传入 `WHALE_PET_MOV_DIR`；`package-macos` 将 MOV 与 WebM 一同放入应用资源目录，安装后无需下载。直接启动二进制时，MOV 默认取素材根的 `mov/`，也可显式指定：

```sh
WHALE_PET_MOV_DIR="$PWD/.tools/macos-assets/mov" target/release/whale-pet
WHALE_PET_VIDEO_BACKEND=webm cargo xtask run
WHALE_PET_VIDEO_BACKEND=hevc cargo xtask run
cargo run --release -p pet-macos --example probe -- .tools/macos-assets/mov/*.mov
```

`WHALE_PET_VIDEO_BACKEND` 接受 `auto`（默认）、`webm`、`hevc`。`auto` 在 MOV 完整且首段硬解可用时启用原生链路，否则记录原因并使用 WebM；`hevc` 要求硬解成功，初始化失败直接报错，便于验证。不能仅凭 Metal 渲染推断硬解；每次创建 VideoToolbox 会话均要求硬件解码，并查询 `UsingHardwareAcceleratedVideoDecoder`，日志出现 `hardware=true` 才是确认结果。系统媒体解码服务必须可访问。

原生路径为 `AVAssetReader` 压缩样本 → `VTDecompressionSession` → BGRA `CVPixelBuffer` / IOSurface → 同一 wgpu Metal 设备的 `CVMetalTextureCache` → GPU 转为 RGBA8 → egui。应用不锁定或读取每帧 CPU 像素，也不经过 `Vec<u8>` 上传。导入纹理同时持有像素缓冲区和 CVMetalTexture，直到 wgpu 退役相应 GPU 资源后释放。

这份固定素材的容器标记是预乘 alpha，但 RGB 实测与原 WebM 的直通 RGB 对应。GPU shader 因此执行一次预乘，并将压缩导致的 1/255 背景残留归零；这项处理针对该素材包，不是任意 MOV 的通用色彩管理。换用其他来源的素材需重新确认 alpha 契约。

打开片段时读取小型压缩样本和完整 PTS 索引，按精确 PTS 顺序输出解码帧，避免依赖 DTS 对重排深度的假设。解码后的表面继续按播放时钟只预取 120 ms，首帧由 UI 消费后才开始计时；动作切换取消旧 worker，跳入片段仍按帧索引解码并丢弃前缀，结束后保留最后一帧。离线 `decode_all` 与姿态元数据生成仍使用 WebM。

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

素材根解析顺序：`--asset-dir` → `WHALE_PET_ASSET_DIR` → `<exe>/assets` → macOS 应用包的 `Contents/Resources/assets` → 仓库内 `dsh-pet/assets`。
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
贴图仅在 `(play_gen, seq)` 变化时重传；同尺寸帧用局部更新复用 GPU 纹理，尺寸变化时才重新创建。
UI 收到第一张保留帧时启动播放时钟，再通知解码线程继续预取，避免打开文件和首帧解码耗时造成开头跳帧。
RGBA 上传前用可向量化的整数预乘转换透明度；输出与锁定 egui 的逐通道转换保持一致。
clip 播放结束后保留最后一帧，拖拽期间即使 clip 播完也保持悬空姿态。

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
- `Ctrl+Q` 退出；macOS 也支持 `Command+Q`

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
甩抛以真实经过时间累计固定 120 Hz 物理步进，显示位置在相邻物理状态之间插值；单次恢复最多累计 250 ms。
着地那一帧按落地前的竖直速度触发压缩（`landingSquash`，越快压得越深）。

自主漫游按 `animations.moves` 选一个动作，用其 `minDist` / `maxDist` / `margin` 规划落点（朝当前面向方向走），
落点越界就不发车；移动在 clip 的 `leadSec` / `tailSec` 之间线性完成，走完提交位置。物理常量全部按
`WORLD_SCALE` 换算到画布像素空间。

### 4.4 交互浮层

宠物与菜单、提示、积分卡片共用一个透明窗口，画布保持 640×360 物理像素。
macOS 日常窗口也是 640×360 物理像素，仅菜单期间扩为 760×480 逻辑点；Linux/Windows 保持扩展窗口。
`PetScene` 统一更新核心状态、计算画布偏移并依次绘制宠物和控件；`SceneLayout` 将外层窗口限制在工作区内，
同时保持画布的目标屏幕位置。各宿主不再创建独立 overlay 窗口或 surface。

- **三种模式**：`Hidden` 不绘制控件，`Transient` 绘制提示或积分，`Menu` 绘制菜单并让整窗接收指针；
  其余模式仅宠物命中区接收输入。菜单期间核心不处理按键边沿，防止菜单点击触发宠物动作。
- **坐标约定**：控件锚点使用物理屏幕坐标，绘制时换算成窗口逻辑点；核心输入则使用画布局部坐标。
  靠近边缘时画布偏移会变化，命中区随之更新，不能假定画布一直居中。
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

按需重绘：解码下一帧到期时间；拖动、甩抛和漫游请求下一帧重绘；Q 弹期间每 8 ms 一次；
按住未拖动时每 8 ms 轮询一次以便窗口外的释放也能被处理；可见控件与宠物在同一绘制周期更新；
Windows 每 16 ms 重绘以轮询全局指针；macOS 由独立的轻量输入采样线程每 16 ms 读取指针，
只有进入/离开命中区、交互区内移动或按住期间的输入变化才唤醒 UI，静止播放按视频 PTS 刷新。

## 5. 平台实现

### 5.1 Linux：原生 Wayland（layer shell）

`crates/pet-wayland` 用 `zwlr_layer_shell_v1` 建窗口。这条路径同时解决了 X11 节里那三个硬需求，且不依赖
XWayland：位置由 anchor + margin 决定，层级天然在普通窗口之上，`wl_surface.set_input_region()` 提供按区域穿透。

- **坐标**：交换链包含画布与控件，尺寸由 `SceneLayout` 按缩放计算；`wp_viewport.set_destination`
  将缓冲映射到窗口逻辑尺寸，`buffer_scale` 保持 1。画布仍保持 640×360 物理像素。
  场景负责原生窗口坐标与画布坐标转换，宿主负责物理像素与输出逻辑坐标转换。
- **缩放**：`wp_fractional_scale_manager_v1` 的 `preferred_scale`（KWin 给 180）除以 120 得到 `ppp`；
  `set_size` + `set_destination` + `set_input_region` 一起重新下发。没有该全局时退回 `wl_output.scale_factor`，
  KWin 只报整数 2，画布会偏大但可用。整数缩放事件不会覆盖已经收到的分数缩放值。
- **定位**：`corner` 直接映射成 `Anchor`，`marginX` / `marginY` 本来就是逻辑像素，原样作为 margin。
  拖动时反向算：`move_to / ppp` 减去输出的 `logical_position` 得到输出内逻辑坐标，再按当前 anchor 换成两个 margin；
  外层窗口由共享布局限位，宿主不再对画布重复限位，以免 margin 与已经绘制的偏移不一致。
- **输出**：以 `wl_surface.enter` 为准——它指出合成器实际把 surface 放在哪个输出上，其 `logical_position` /
  `logical_size` 供全局坐标换算与共享布局使用。只有在该事件始终没来时，才退回「第一个报告逻辑尺寸的输出」。
- **穿透**：`set_input_region` 限制为 `HIT_BOX`（按 `ppp` 换算），命中框外的点击落到下层窗口。
- **拖动**：不锁指针。窗口跟随指针时指针仍在 surface 内，`wl_pointer` 的隐式抓取足够；全局位置只在收到指针事件时
  按当时的窗口原点换算并缓存——Wayland 不会因为窗口自己移动而补发 motion，每帧重算会形成反馈。释放速度的轨迹
  采样同样只认「指针真的动了」，理由与实测见 4.3 与 7.5。
- **键盘**：`KeyboardInteractivity::OnDemand`；KWin 会在指针进入后给焦点（`wl_keyboard.enter` 实测到达）。
  `Ctrl+Q` 由 `wl_keyboard.press_key` 的 keysym 直接判定，不依赖 egui 的键映射。
- **渲染**：wgpu 30.0.1 的 Vulkan 后端；优先 `Bgra8Unorm` + `PreMultiplied` alpha。`Queue::present` 在 Wayland 上
  attach + commit 一次完成，所以 viewport destination、margin、input region 都必须在 present 之前设置。
- **重绘**：`FullOutput.viewport_output[ROOT].repaint_delay` 决定下一次唤醒，`Duration::MAX` 时用 1 秒心跳兜底。
- **交互控件**：与宠物共享 layer surface、egui context 和 GPU 渲染器。菜单打开时输入区为整窗，
  关闭后恢复带画布偏移的命中区；相同输入区不重复下发。Esc 直接进入同一 egui 输入队列。
- **指针归属**：只处理当前宠物 surface 的事件；离开时清理悬停指针，拖动期间保留释放所需的位置。

### 5.2 Linux / X11

设置了 `DISPLAY` 且未传 `--force-x11` 时使用 X11（自动回退或显式指定）：KWin 的原生 Wayland toplevel 无法自我定位、
置顶或设置输入区域。

- 输入区域：用 X11 SHAPE 扩展把窗口输入区限制为命中框，透明区域天然点击穿透
- 全局指针：直接 `query_pointer` 读根窗口坐标与按键，因为指针一旦拖出输入区，窗口就收不到后续移动与释放事件
- 工作区：读 `_NET_WORKAREA` 的当前桌面项（多屏时为各屏并集），与窗口所在显示器矩形取交集；取不到时退回显示器矩形
- 启动定位：KWin 会忽略窗口映射前发出的移动请求，所以启动后 2 秒内每 500 ms 重发一次位置与置顶，
  用户开始拖动后停止重发
- 交互控件：与宠物共享 eframe 根视口。X11 SHAPE 随菜单状态在整窗和偏移后的宠物命中区之间切换，
  相同输入区不重复下发。

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

交互控件：与宠物共用根窗口和 DX12 交换链。菜单打开时整窗可交互，其余时候按偏移后的宠物命中区切换穿透。
窗口显式关闭阴影，vendored egui-winit 在创建无边框窗口时尊重 `has_shadow`。

### 5.4 macOS

使用 eframe/wgpu 的原生 Metal 窗口，宠物与菜单共用同一个透明、无边框、置顶窗口。
事件循环采用 `ActivationPolicy::Accessory`，不在 Dock 中添加应用图标，也不在启动时抢占其他应用焦点。

`macos.rs` 使用 objc2 AppKit 的安全接口读取 `NSEvent.mouseLocation` 与 `pressedMouseButtons`，
即使窗口正在穿透，也能继续跟踪指针和按键。穿透策略与 Windows 共用：命中框外切换
`ViewportCommand::MousePassthrough`（winit 映射为 `NSWindow.ignoresMouseEvents`），菜单打开或拖动按住时保持交互。
输入采样保持每 16 ms 一次，进入命中框时可补齐位置及按下边沿；采样本身不触发持续绘制，线程随窗口退出停止并回收。

位置唯一归属 `PetCore` 的视频画布坐标。首次显示先由 core 按配置和工作区初始化画布，再反算窗口位置；
拖动、甩抛、漫游、回到初始位置、菜单切换和缩放都沿用这条链路。
窗口原点 = 画布原点 − 画布在窗口内的偏移，命中判断 = 全局指针 − 画布原点。
native 窗口位置读回只用于检查/重试输出命令，不回填视频位置。抛掷期间内部偏移保持 (0,0)，
避免原生窗口平移与反向视频偏移异步呈现导致的错位。菜单展开/收起保持画布屏幕位置不变。
macOS 画布输出按 backing scale 对齐至完整逻辑点，再由它统一推导窗口和输入位置；
本机 2× Retina 对半逻辑点窗口位置会取整，否则奇数物理像素目标会导致不断重试。
同一目标最多重试三次，菜单尺寸或缩放变化时重新核验；不把系统读回位置写回画布。

工作区使用当前显示器的 `NSScreen.visibleFrame`，排除菜单栏与 Dock。Cocoa 点坐标先按主屏高度翻转 y，
再乘当前窗口的 backing scale，与 winit 的 `outer_position` / `set_outer_position` 使用同一物理坐标域。
这也适用于非主屏的负坐标；混合缩放多屏的实际拖动仍需对应硬件复验。

### 5.5 vendored egui-winit

`third_party/egui-winit/` 是 egui-winit 0.37.0 的原始副本，通过根 `Cargo.toml` 的 `[patch.crates-io]` 覆盖。
本地补丁提供 `set_transparent_no_redirection_bitmap`，并让 Windows 无边框窗口尊重 `has_shadow`。
`.cargo_vcs_info.json` 记录上游提交
`7c0b40402271e8d40cda69f87cf3c2617c2e4382`，便于比对。

## 6. 已知限制

- 原生 Wayland 的热插拔与跨屏：layer surface 绑定单个输出，输出消失时停在最后已知几何，不会重建 surface
- 原生 Wayland 的拖动起点必须在命中框内：输入区外的点击不产生按下事件（这正是穿透的实现方式）
- 原生 Wayland 需要合成器提供 `zwlr_layer_shell_v1`；不提供时自动回退 XWayland
- 菜单窗口为 760×480 逻辑点：三级菜单在贴边时会夹在窗口内，超长列表靠滚动查看
- 指纹/触摸、输入法未接入；只处理鼠标左键/右键与退出快捷键（macOS 为 `Ctrl+Q` / `Command+Q`，其余平台为 `Ctrl+Q`；菜单打开时额外处理 Esc）
- `--stats` 仅 Linux 实现
- 姿态匹配只决定从哪一帧进入：没有过渡动作或形变，切换代价高的组合仍是硬切。自然结束的切换会多等一次
  ui pass，因为末帧要先绘制一次
- `anim-meta.json` 必须与素材一同发布；缺少它时所有切换都从第 0 帧开始（仍有硬切，没有淡入）。素材被换成更短的
  版本而元数据未重生成时，若选中的入场帧超出新片长，该 clip 会打 warning 并退回第 0 帧重播，不会计入解码失败
- macOS 已移除飞行中同时移动窗口与反向移动画布的路径，代表性合成录制的轨迹偏差下降；素材自身动画、系统合成节奏和采集丢帧仍会影响帧间观感，不宣称所有显示器上逐帧零抖动
- 未验证：小时级连续播放（解码线程与内存的长期行为），以及「素材替换但元数据未重生成」
  的现实场景（只验证了截短片长的构造场景与单 clip 校验）
- macOS 提供本机架构的 DMG 打包入口；尚无 Developer ID 签名、公证、自动更新或其他平台的安装包
- 只解码视频轨，没有音频
- 未接入 dsh：余额、碎碎念、对话、系统通知、工作状态联动、HTTP 端点、占用锁、斜杠命令、设置页、托盘
  以及菜单里的 open-site / reload / set-display 都不存在；`animations.events` 只解析不使用

## 7. 验证记录

macOS 14.8.9 / Apple Silicon / 2× 单屏完成 debug 与 release 构建、正式启动、透明动画、单个可见置顶窗口、
底层窗口点击穿透、点击回应、右键级联菜单、慢拖停住、快甩与 Ctrl+Q / Command+Q 退出验证。
`cargo xtask check`（格式、Clippy、全部已有 workspace 测试）通过；`anim-meta` 完整解码 106 段并回读自检通过。
本次未覆盖 Intel macOS、多屏混合缩放及热插拔。

以下 7.1–7.5 保留早期独立浮层实现的历史记录，其中窗口数量与尺寸不代表当前单窗口设计。
当前单窗口版本在 Windows 虚拟机完成菜单浏览、慢拖停住与快甩复验，窗口枚举仅有一个可见宠物顶层窗口。
Linux 已完成 X11 与原生 Wayland 的交互检查；独立 KWin 虚拟输出在宿主锁屏时通过 EIS 注入快甩，
并取得合成截图与落地日志。逐张截图只有约 4–5 fps，不作为偶发单帧震动已消失的证明。

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
