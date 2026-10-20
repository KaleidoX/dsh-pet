# dsh-pet 🐳

这是 [PC2005-cloud/dsh-pet](https://github.com/PC2005-cloud/dsh-pet) 的衍生仓库，在原项目基础上加入了 **Codex 桌面应用**的自定义桌宠包——**鲸鱼娘（Whale Girl）**。

> **维护范围**：本仓库**不负责** dsh-pet 插件本身，也**不负责**原项目的桌面端（Electron 透明窗）。插件的安装、配置、问题反馈与发版，请前往原项目：
> <https://github.com/PC2005-cloud/dsh-pet>

## 🐳 Codex 桌宠包（鲸鱼娘 / Whale Girl）

角色取自本项目的透明动画素材：Q 版蓝发小女仆——头顶呆毛、耳侧鳍饰，身后拖着鲸鱼尾巴。宠物包内含现成图集，也含可复现的构建脚本（抽帧 → 拼 1536×2288 图集 → 官方校验 → 安装）。

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
