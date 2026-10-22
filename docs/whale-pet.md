# whale-pet 技术说明

面向维护者的实现说明：工作区结构、构建、运行时机制与两个平台的窗口处理。
产品介绍与使用方式见仓库根目录的 [README](../README.md)。

## 1. 工作区结构

| 路径 | 职责 |
| --- | --- |
| `crates/pet-config` | 解析上游 `config.jsonc` 的动画池、`physics.throwPower`、`pets[0].position`；画布与交互常量；素材目录解析 |
| `crates/pet-model` | 纯逻辑：动画池选择、拖动弹簧、Q 弹曲线 |
| `crates/pet-decode` | `ffmpeg-next` 流式解码 VP9-alpha WebM，按 PTS 交付 RGBA 帧 |
| `crates/pet-ui-core` | 与窗口系统无关的行为：动画池、解码推进、弹簧拖动、Q 弹、命中测试、贴图、重绘节奏、`--stats` 计量 |
| `crates/pet-ui-egui` | eframe 宿主：窗口、egui 事件循环、平台集成（X11 / Windows） |
| `crates/pet-wayland` | 原生 Wayland 宿主：`zwlr_layer_shell_v1` + `wgpu` + `egui-wgpu` + `calloop`（仅 Linux） |
| `crates/pet-win32` | Windows 全局指针读取与 DWM 边框关闭；唯一直接调用 Win32 的 crate |
| `apps/whale-pet` | 二进制入口：参数解析、素材定位、宿主选择 |
| `xtask` | `build-ffmpeg` / `run` / `check` 命令 |

依赖方向：`pet-config → pet-model → pet-decode → pet-ui-core → pet-ui-egui / pet-wayland → apps/whale-pet`；`pet-win32` 只在 Windows 目标上被 `pet-ui-egui` 引用，`pet-wayland` 只在 Linux 目标上被 `apps/whale-pet` 引用。

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
| `--force-x11` | 跳过原生 Wayland，强制 X11/XWayland 后端（见 5.1） |
| `--force-wayland` | 只用原生 Wayland；初始化失败直接报错而不回退（调试用） |
| `--no-force-x11` | 兼容性 no-op：「不强制 X11」现在已是默认行为 |
| `-h, --help` | 帮助 |

Wayland 会话（存在 `WAYLAND_DISPLAY`）下默认先走原生 layer shell（见 5.1），初始化失败则记 warning 并
强制 XWayland 重试——回退时会显式选中 X11，否则 winit 会再次挑中 Wayland 并以同样原因失败。

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
拖动开始后停止重发启动定位（见 5.2）。

### 4.4 重绘调度

按需重绘：解码下一帧到期时间；拖动中每帧重绘；按住未拖动时每 8 ms 轮询一次以便窗口外的释放也能被处理；
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
  按当时的窗口原点换算并缓存——Wayland 不会因为窗口自己移动而补发 motion，每帧重算会形成反馈。
- **键盘**：`KeyboardInteractivity::OnDemand`；KWin 会在指针进入后给焦点（`wl_keyboard.enter` 实测到达）。
  `Ctrl+Q` 由 `wl_keyboard.press_key` 的 keysym 直接判定，不依赖 egui 的键映射。
- **渲染**：wgpu 30.0.1 的 Vulkan 后端；优先 `Bgra8Unorm` + `PreMultiplied` alpha。`Queue::present` 在 Wayland 上
  attach + commit 一次完成，所以 viewport destination、margin、input region 都必须在 present 之前设置。
- **重绘**：`FullOutput.viewport_output[ROOT].repaint_delay` 决定下一次唤醒，`Duration::MAX` 时用 1 秒心跳兜底。

### 5.2 Linux / X11

设置了 `DISPLAY` 且未传 `--force-x11` 时使用 X11（自动回退或显式指定）：KWin 的原生 Wayland toplevel 无法自我定位、
置顶或设置输入区域。

- 输入区域：用 X11 SHAPE 扩展把窗口输入区限制为命中框，透明区域天然点击穿透
- 全局指针：直接 `query_pointer` 读根窗口坐标与按键，因为指针一旦拖出输入区，窗口就收不到后续移动与释放事件
- 启动定位：KWin 会忽略窗口映射前发出的移动请求，所以启动后 2 秒内每 500 ms 重发一次位置与置顶，
  用户开始拖动后停止重发

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

### 5.4 vendored egui-winit

`third_party/egui-winit/` 是 egui-winit 0.37.0 的原始副本，通过根 `Cargo.toml` 的 `[patch.crates-io]` 覆盖。
唯一改动是新增 `set_transparent_no_redirection_bitmap`。`.cargo_vcs_info.json` 记录上游提交
`7c0b40402271e8d40cda69f87cf3c2617c2e4382`，便于比对。

## 6. 已知限制

- 原生 Wayland 的热插拔与跨屏：layer surface 绑定单个输出，输出消失时停在最后已知几何，不会重建 surface
- 原生 Wayland 的拖动起点必须在命中框内：输入区外的点击不产生按下事件（这正是穿透的实现方式）
- 原生 Wayland 需要合成器提供 `zwlr_layer_shell_v1`；不提供时自动回退 XWayland
- 指纹/触摸、输入法未接入；只处理鼠标左键与 `Ctrl+Q`
- `--stats` 仅 Linux 实现
- 没有安装包与发布流程，只能从源码构建
- 只解码视频轨，没有音频

## 7. 验证记录

原生 Wayland（KDE Wayland 6.3.6，DP-2 缩放 1.5，debug 构建）：日志确认 `fractional scale 1.5` →
`427x240 logical`、`surface on DP-2 at (1440, 560)`；协议日志确认 `set_size(427, 240)`、
`wp_viewport.set_destination(427, 240)`、`set_input_region` 为命中框 `(133, 33, 160, 190)`（逻辑像素 = 物理 / 1.5）、
`preferred_scale(180)`，以及 `OnDemand` 下 `wl_keyboard.enter` 到达。系统截图显示宠物在配置的右上角、
背景透出下层窗口、无窗口装饰。稳定 24 fps（与素材帧率一致）、CPU 3–4%、RSS 约 163 MB、贴图 0.88 MB。
回退链实测：`WAYLAND_DISPLAY` 指向不存在的 socket 时记 `native Wayland failed ... falling back to XWayland`
并成功落到 X11（640×360 物理像素、scale 1.5）；`--force-wayland` 在同条件下直接报错退出（exit 1）。
本机没有 `xdotool` / `ydotool` / `wtype`，`/dev/uinput` 仅 root 可写，点击、拖动与 `Ctrl+Q` 未做自动化注入验证，
只有上述协议层证据。

X11（KDE Wayland + XWayland，DP-2 缩放 1.5，release）：稳定 24 fps（与素材帧率一致）、CPU 5–6%、
RSS 约 154 MB、贴图 0.88 MB；命中框内外的点击分别由宠物与下层探针窗口收到；Q 弹测量 271 → 148 → 279 px；
拖动窗口跟随指针，松手后保留位置。

Windows（QEMU/KVM，Windows 11 22631，虚拟机内为 WARP 软件光栅）：per-pixel 透明、四个方位的点击穿透、
50/100/200/400 ms 快速点击、拖动均通过；工作集 28.3 MB、峰值 65.6 MB、私有提交 297.8 MB、13 线程、单核约 38%。
软件光栅下的 CPU 与内存不代表有 GPU 的真实机器。
