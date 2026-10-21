# whale-pet 技术说明

面向维护者的实现说明：工作区结构、构建、运行时机制与两个平台的窗口处理。
产品介绍与使用方式见仓库根目录的 [README](../README.md)。

## 1. 工作区结构

| 路径 | 职责 |
| --- | --- |
| `crates/pet-config` | 解析上游 `config.jsonc` 的动画池、`physics.throwPower`、`pets[0].position`；画布与交互常量；素材目录解析 |
| `crates/pet-model` | 纯逻辑：动画池选择、拖动弹簧、Q 弹曲线 |
| `crates/pet-decode` | `ffmpeg-next` 流式解码 VP9-alpha WebM，按 PTS 交付 RGBA 帧 |
| `crates/pet-ui-egui` | egui 窗口、贴图、输入处理、平台集成（X11 / Windows） |
| `crates/pet-win32` | Windows 全局指针读取与 DWM 边框关闭；唯一直接调用 Win32 的 crate |
| `apps/whale-pet` | 二进制入口：参数解析、素材定位、eframe 启动 |
| `xtask` | `build-ffmpeg` / `run` / `check` 命令 |

依赖方向：`pet-config → pet-model → pet-decode → pet-ui-egui → apps/whale-pet`；`pet-win32` 只在 Windows 目标上被 `pet-ui-egui` 引用。

workspace 级 lint 为 `unsafe_code = "forbid"`。`pet-win32` 有意不继承 workspace lints：它需要的 `GetCursorPos` / `GetAsyncKeyState` / `DwmSetWindowAttribute` 在 winit 0.30 中没有安全等价物，`unsafe` 只集中在这一个 crate。

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
cargo xtask check    # rustfmt --check + clippy -D warnings
```

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
| `--no-force-x11` | 不强制 X11/XWayland 后端（见 5.1） |
| `-h, --help` | 帮助 |

素材根解析顺序：`--asset-dir` → `WHALE_PET_ASSET_DIR` → `<exe>/assets` → 仓库内 `dsh-pet/assets`。
`config.jsonc` 读取失败时回退默认配置并告警；动画池为空时用内置默认动画名。

窗口位置取 `pets[0].position` 的 `corner`（`top-left` / `top-right` / `bottom-left` / `bottom-right`）与
`marginX` / `marginY`（逻辑像素），按主显示器计算，画布 640×360 物理像素。

## 4. 运行时

### 4.1 解码与播放

每个 clip 一个解码线程，线程名 `pet-decode`。解码器固定用 `libvpx-vp9`：按 codec id 解析到的默认 `vp9`
解码器会丢掉 WebM 的 alpha 边流。帧转成紧密排布的 RGBA8，保持素材原生分辨率（640×360），不裁剪、不缩放。

解码线程按帧的 PTS 与播放时钟交付，最多提前 120 ms 排队；UI 一次只持有一帧，内存随分辨率而非时长增长。
贴图仅在 `(play_gen, seq)` 变化时重传。clip 播放结束后保留最后一帧，拖拽期间即使 clip 播完也保持悬空姿态。

播放规则与上游网页客户端一致：点击池随机且不排除当前 clip；待机池排除当前 clip；拖动与点击播完一遍后回待机。
连续 3 个 clip 一帧都没解出时停止播放（`stalled`），避免空转。

### 4.2 交互

- 命中框：画布坐标 `x 200..440`、`y 50..335`（`HIT_BOX`）
- 按下后位移达到 5 逻辑像素（`DRAG_THRESHOLD`）即进入拖动，否则松手算点击
- 点击回应带 Q 弹：0.55 倍纵向压缩，随后 ease-out-back 回弹，上限 1.12，总时长 220 ms，底边为锚点
- 光标在命中框上显示 `Grab`，拖动中显示 `Grabbing`
- `Ctrl+Q` 退出

### 4.3 拖动

窗口位置由两轴弹簧积分：`K = 200`、`C = 30`，`physics.throwPower` 作为力的倍率；`dt` 截断到 1/30 秒。
弹簧目标 = 全局指针 − 按下时的指针偏移 − 画布中心，积分结果直接写入 `set_outer_position`。
拖动开始后停止重发启动定位（见 5.1）。

### 4.4 重绘调度

按需重绘：解码下一帧到期时间；拖动中每帧重绘；按住未拖动时每 8 ms 轮询一次以便窗口外的释放也能被处理；
Windows 额外固定每 16 ms 轮询全局指针（见 5.2）。

## 5. 平台实现

### 5.1 Linux / X11

默认强制 XWayland 后端（设置了 `DISPLAY` 且未传 `--no-force-x11` 时）：KWin 的原生 Wayland 客户端无法自我定位、
置顶或设置输入区域，而这三点都是桌宠的硬需求。

- 输入区域：用 X11 SHAPE 扩展把窗口输入区限制为命中框，透明区域天然点击穿透
- 全局指针：直接 `query_pointer` 读根窗口坐标与按键，因为指针一旦拖出输入区，窗口就收不到后续移动与释放事件
- 启动定位：KWin 会忽略窗口映射前发出的移动请求，所以启动后 2 秒内每 500 ms 重发一次位置与置顶，
  用户开始拖动后停止重发

### 5.2 Windows

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

### 5.3 vendored egui-winit

`third_party/egui-winit/` 是 egui-winit 0.37.0 的原始副本，通过根 `Cargo.toml` 的 `[patch.crates-io]` 覆盖。
唯一改动是新增 `set_transparent_no_redirection_bitmap`。`.cargo_vcs_info.json` 记录上游提交
`7c0b40402271e8d40cda69f87cf3c2617c2e4382`，便于比对。

## 6. 已知限制

- 原生 Wayland 不支持：点击穿透依赖 X11 SHAPE；强制 X11 是当前的取舍
- Linux 首次可见后有 0.5–1 秒的跳位（KWin 先自行摆放），启动时窗口会带 `_NET_WM_STATE_DEMANDS_ATTENTION`
- `--stats` 仅 Linux 实现
- 没有安装包与发布流程，只能从源码构建
- 只解码视频轨，没有音频

## 7. 验证记录

Linux（KDE Wayland + XWayland，DP-2 缩放 1.5，release）：稳定 24 fps（与素材帧率一致）、CPU 5–6%、
RSS 约 154 MB、贴图 0.88 MB；命中框内外的点击分别由宠物与下层探针窗口收到；Q 弹测量 271 → 148 → 279 px；
拖动窗口跟随指针，松手后保留位置。

Windows（QEMU/KVM，Windows 11 22631，虚拟机内为 WARP 软件光栅）：per-pixel 透明、四个方位的点击穿透、
50/100/200/400 ms 快速点击、拖动均通过；工作集 28.3 MB、峰值 65.6 MB、私有提交 297.8 MB、13 线程、单核约 38%。
软件光栅下的 CPU 与内存不代表有 GPU 的真实机器。
