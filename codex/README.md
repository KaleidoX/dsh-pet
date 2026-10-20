# Codex 桌宠包（鲸鱼娘 / Whale Girl）

把本项目「蓝发小女仆」的透明动画打包成 [Codex 桌面应用](https://chatgpt.com/codex) 可直接加载的**自定义宠物包**。
角色与 DSH 插件里的是同一只：Q 版蓝发小女仆——头顶呆毛、耳侧鳍饰，身后拖着鲸鱼尾巴，因此命名为**鲸鱼娘（Whale Girl）**。

Codex 的宠物是一张 1536×2288 的精灵图集（8 列 × 11 行、每格 192×208），前 9 行是标准动作、后 2 行是 16 个看向方向。本目录提供了**打包好的成品**和**可复现的构建脚本**。

```
codex/
├─ build_codex_pet.py                  # 构建/安装脚本
├─ opts.example.jsonc                  # 本地配置模板（复制为 opts.jsonc 使用，不入库）
└─ pets/whale-girl/
   ├─ pet.json                         # Codex 宠物清单（id / 名称 / 图集版本）
   ├─ spritesheet.webp                 # 成品图集（1536×2288，v2）
   ├─ pet.spec.json                    # 构建规格（每个动作取哪个源动画的哪几帧）
   └─ preview.png                      # 静态预览
```

## 一、安装到 Codex

装好 [Codex 桌面应用](https://chatgpt.com/codex) 后，在仓库根目录执行：

```sh
python3 codex/build_codex_pet.py install
```

脚本把 `codex/pets/whale-girl/` 复制到 `${CODEX_HOME:-~/.codex}/pets/whale-girl/`。
`install` 只做文件复制，不依赖 Pillow、numpy 或 ffmpeg，系统自带的 `python3` 即可。

然后打开 Codex 的 Pets（宠物）设置页刷新，列表里会出现「鲸鱼娘」，选中即可。
Windows 上把 `python3` 换成 `python`。

安装位置与 `CODEX_HOME` 一致：

| 系统 | 默认路径 |
| --- | --- |
| Linux / macOS | `~/.codex/pets/whale-girl/` |
| Windows | `%USERPROFILE%\.codex\pets\whale-girl\` |

不想用脚本时，手动把 `pets/whale-girl/` 整个目录复制进上面的 `pets/` 目录，效果相同。
`pet.json` 只需要这几个字段，`spritesheetPath` 相对于宠物目录：

```json
{
  "id": "whale-girl",
  "displayName": "鲸鱼娘",
  "description": "Q版蓝发小女仆，头顶呆毛、耳侧鳍饰，身后拖着鲸鱼尾巴。",
  "spriteVersionNumber": 2,
  "spritesheetPath": "spritesheet.webp"
}
```

> 图集尺寸必须**正好** 1536×2288（v2）或 1536×1872（v1），否则应用会拒绝加载。本包是 v2。

## 二、动作映射

Codex 每个标准行帧数固定，本包的素材对应关系：

| 行 | Codex 状态 | 使用的 DSH 动画 |
| --- | --- | --- |
| 0 | `idle` 待机 | 待机呼吸休闲 |
| 1 | `running-right` 向右跑 | 原地左转奔跑（镜像） |
| 2 | `running-left` 向左跑 | 原地左转奔跑 |
| 3 | `waving` 挥手 | 点击回应-元气挥手 |
| 4 | `jumping` 跳跃 | 点击回应-开心跃动 |
| 5 | `failed` 失败 | 工作状态-垂头叹气冒汗 |
| 6 | `waiting` 等待 | 工作状态-思考冒泡 |
| 7 | `running` 执行中 | 写代码 |
| 8 | `review` 审阅 | 工作状态-清点归档 |
| 9-10 | 16 个看向方向 | 小幅度原地 360 度旋转展示 |

没被用到的格子（每行第 N 帧之后）全部保持全透明，这是 v2 校验的硬性要求。

## 三、配置

脚本按「命令行参数 > 环境变量 > 配置文件 > 内置默认值」取值。日常只需要配置文件：

```sh
cp codex/opts.example.jsonc codex/opts.jsonc   # 一次即可，之后按需修改
```

`codex/opts.jsonc` 只属于本机且已在 `.gitignore` 中，不会进仓库。可用字段：

| 字段 | 作用 | 默认 |
| --- | --- | --- |
| `codexHome` | 安装根目录，包写到 `<codexHome>/pets/<id>/` | `~/.codex` |
| `validateAtlas` | 官方 `validate_atlas.py` 的路径；留空则跳过官方校验 | 空 |
| `cacheDir` | 从 webm 抽帧的缓存目录 | `.agents/tmp/codex-pet-frames` |
| `outDir` | 构建产物目录 | `.agents/tmp/codex-pet-out` |
| `install` | `build` 后是否自动安装 | `true` |
| `forceExtract` | 忽略帧缓存、每次重新抽帧 | `false` |

文件支持 `//` 与 `/* */` 注释。也可以临时改用 `--config other.jsonc` 指定另一份配置。

对应的环境变量（优先级更高，适合一次性覆盖）：

| 环境变量 | 对应字段 |
| --- | --- |
| `CODEX_HOME` | `codexHome` |
| `CODEX_VALIDATE_ATLAS` | `validateAtlas` |
| `CODEX_PET_CACHE` | `cacheDir` |
| `CODEX_PET_OUT` | `outDir` |
| `CODEX_PET_INSTALL` | `install` |
| `CODEX_PET_FORCE_EXTRACT` | `forceExtract` |

例如把宠物装到别处：

```sh
CODEX_HOME=/path/to/codex-home python3 codex/build_codex_pet.py install
```

## 四、从源码重新构建

构建需要 Python 3、`pillow`、`numpy`，以及 `ffmpeg`（从 webm 源动画抽帧）：

```sh
pip install pillow numpy
python3 codex/build_codex_pet.py build codex/pets/whale-girl/pet.spec.json
```

脚本流程：读取 spec → 用 ffmpeg 把源 webm 抽成 PNG 序列（缓存在 `cacheDir`）→ 按 spec 逐帧裁剪、缩放、镜像、对齐地面线 → 拼成 1536×2288 图集并导出无损 WebP → 校验 → 写入 `<codexHome>/pets/<id>/`。

常用子命令：

```sh
python3 codex/build_codex_pet.py install [包目录]              # 安装成品包（默认 codex/pets/whale-girl）
python3 codex/build_codex_pet.py inspect <spec>                # 只解析并打印每行取帧与几何参数
python3 codex/build_codex_pet.py build   <spec>                # 构建 + 校验 + 安装
python3 codex/build_codex_pet.py build   <spec> --no-install --out /tmp/atlas
python3 codex/build_codex_pet.py validate <atlas.webp>         # 单独校验已有图集
```

`pet.spec.json` 里的 `sourceDir` 指向本仓库的 `dsh-pet/assets/webm/`，所以构建要在**仓库根目录**执行；把 spec 拷到别处时把它改成绝对路径即可。

官方 `validate_atlas.py` 随 Codex 桌面应用发布，各平台安装方式不同，常见位置：

```sh
# Linux（deb/rpm 安装）
/usr/lib/chatgpt/resources/skills/skills/.curated/hatch-pet/scripts/validate_atlas.py
# macOS
/Applications/ChatGPT.app/Contents/Resources/skills/skills/.curated/hatch-pet/scripts/validate_atlas.py
# Windows
C:\Users\<你>\AppData\Local\Programs\ChatGPT\resources\skills\skills\.curated\hatch-pet\scripts\validate_atlas.py
```

把它填进 `codex/opts.jsonc` 的 `validateAtlas`（或设 `CODEX_VALIDATE_ATLAS`），构建时会自动校验；不填则跳过。

## 五、换一只宠物 / 调整动作

spec 是纯数据，复制一份改名字和动作表即可：

```jsonc
{
  "id": "my-pet",                       // 安装目录名
  "displayName": "我的宠物",
  "sourceDir": "dsh-pet/assets/webm",   // 源动画目录（也可放 PNG 序列：<sourceDir>/<名>/*.png）
  "ground": 330,                        // 源帧里的地面线 y
  "rows": {
    "0": { "animation": "待机呼吸休闲", "frames": [1, 18, 36, 50, 60, 76],
           "axis": 314, "scale": 0.66 }
    // rows 1-8 同理；每行帧数固定：6 / 8 / 8 / 4 / 5 / 8 / 6 / 6 / 6
  },
  "look": { "animation": "小幅度原地360度旋转展示", "frames": [/* 16 个帧号 */],
            "axis": 322, "scale": 0.63 }
}
```

参数含义：

- `axis`：角色中轴在源帧里的 x，决定图集里角色的水平位置；写 `"auto"` 用 alpha 加权重心估算。
- `scale`：源帧到单元格的缩放；写 `"fit"` 按角色包围盒自动适配（留白由 `fitPadding` 控制，默认 4px）。
- `ground`：源帧里的地面线 y；写 `"auto"` 取所选帧包围盒的底边。
- `base`：地面线落在单元格内的 y（标准行默认 199、看向行 191），按行可覆盖。
- `frames`：从 1 开始的帧号列表，或 `"even"` 在整段里等距取样（含首尾）。
- `mirror`：左右镜像，例如用同一段朝左的奔跑帧生成 `running-right`。

不熟悉的素材可以直接全自动起步：

```jsonc
{ "ground": "auto", "axis": "auto", "scale": "fit",
  "rows": { "0": { "animation": "待机呼吸休闲", "frames": "even" }, ... },
  "look": { "animation": "小幅度原地360度旋转展示", "frames": "even" } }
```

先用 `inspect` 看脚本算出来的取帧和缩放是否合理，再按需手调。

## 六、说明

- 图集不透明区域之外的像素 RGB 全部清零，否则官方校验器会报 `transparent RGB residue`。
- 同一段动画里角色若有位移，应选位移小的帧；Codex 只会按格播放，不会自动抵消位移。
- 「看向方向」需要一段原地旋转的素材，按顺时针每 22.5° 取一帧，顺序错会导致宠物看向错误方向。
- 成品图集与 DSH 插件共用同一批素材，授权同仓库 [`LICENSE`](../LICENSE)。
