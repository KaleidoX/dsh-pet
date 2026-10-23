# whale-pet 🐳

**whale-pet** 是基于 egui 的跨平台独立桌面桌宠：单窗口、640×360 原尺寸透明动画，支持待机、拖动、甩抛、自主漫游、点击回应、悬浮提示与右键菜单。角色是 **鲸鱼娘（Whale Girl）**——Q 版蓝发小女仆，头顶呆毛、耳侧鳍饰，身后拖着鲸鱼尾巴。

本仓库同时是 [PC2005-cloud/dsh-pet](https://github.com/PC2005-cloud/dsh-pet) 的衍生仓库，保留了上游插件源码与其素材，并额外提供 **Codex 桌面应用**的自定义桌宠包。

> **维护范围**：本仓库**不负责** dsh-pet 插件本身，也**不负责**原项目的桌面端（Electron 透明窗）。插件的安装、配置、问题反馈与发版，请前往原项目：
> <https://github.com/PC2005-cloud/dsh-pet>

## 🖥️ 独立桌面应用（whale-pet）

Rust + egui 实现的独立桌宠：单窗口、置顶、透明，直接播放上游的 VP9-alpha WebM，不依赖 Node 或 Electron。只有人物区域接收点击，其余部分的点击会穿透到下层窗口。支持 Linux（Wayland 原生 layer shell 与 X11/XWayland）与 Windows。

```sh
cargo xtask build-ffmpeg --target linux   # 准备解码库
cargo xtask run                           # 构建并启动
```

```sh
whale-pet [--asset-dir <DIR>] [--stats] [--force-x11 | --force-wayland]
```

拖动可移动窗口，快速甩动会把宠物抛出去；点击命中框会被回应（飞行中点击还会弹出积分卡片），
指针悬停显示名字，右键打开动作菜单。`Ctrl+Q` 退出。素材根默认取仓库内的 `dsh-pet/assets`，也可以用
`--asset-dir` 或 `WHALE_PET_ASSET_DIR` 指定位置。Wayland 会话下默认走原生 layer shell，初始化失败会自动回退 XWayland。

菜单、悬浮提示与积分卡片和宠物画在同一个透明窗口里，各平台均只创建一个宠物窗口。宠物画布保持 640×360 物理像素，外层窗口至少为 760×480 逻辑点，为菜单留出透明余量；菜单打开时优先接收输入，关闭后恢复宠物命中区之外的点击穿透。
本应用不接入 dsh：余额、碎碎念、对话、系统通知与工作状态联动都不包含在内。

构建细节、平台实现限制与实测数据见 [`docs/whale-pet.md`](docs/whale-pet.md)。

## 🐳 Codex 桌宠包（鲸鱼娘 / Whale Girl）

角色取自本项目的透明动画素材。宠物包内含现成图集，也含可复现的构建脚本（抽帧 → 拼 1536×2288 图集 → 官方校验 → 安装）。

在仓库根目录执行：

```sh
python3 codex/build_codex_pet.py install
```

脚本会把 `codex/pets/whale-girl/` 安装到 `${CODEX_HOME:-~/.codex}/pets/whale-girl/`，之后在 Codex 的 Pets 设置页刷新并选中「鲸鱼娘」即可。`install` 只复制文件，不依赖额外 Python 包。

动作映射、配置项、自行构建与换装方法详见 [`codex/README.md`](codex/README.md)。

## 许可

- 代码：MIT（见 [`LICENSE`](LICENSE)）
- 素材（动画 / 提示词 / 源视频）：允许开源使用，**禁止商用**
- **二次创作约定**：基于本项目的衍生 / 改版 / 换皮作品，在**任何介绍、展示、分发该作品的地方**，须附上原作者 GitHub 地址：<https://github.com/PC2005-cloud/dsh-pet>
