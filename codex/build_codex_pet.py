#!/usr/bin/env python3
"""把 dsh-pet 的透明动画转换成 Codex v2 宠物包。

输入是一份 JSON 规格（示例见 codex/pets/whale-girl/pet.spec.json）：
  - rows：Codex 行 0-8 各用哪个源动画、取哪几帧、角色中轴 x、缩放、是否镜像；
  - look：16 个看向方向用哪个动画（通常是原地转盘），按顺时针顺序取帧。
源动画可以是 .webm（用 ffmpeg 抽帧）或 PNG 序列目录。

输出 1536×2288 的 8×11 RGBA atlas（9 个标准行 + 2 行看向方向），
用官方 validate_atlas.py --require-v2 校验后安装到
${CODEX_HOME:-$HOME/.codex}/pets/<id>/{pet.json,spritesheet.webp}。

用法：
  python codex/build_codex_pet.py install  codex/pets/whale-girl
  python codex/build_codex_pet.py build    codex/pets/whale-girl/pet.spec.json
  python codex/build_codex_pet.py inspect  codex/pets/whale-girl/pet.spec.json
  python codex/build_codex_pet.py build    spec.json --out /tmp/atlas --no-install
  python codex/build_codex_pet.py validate /path/to/spritesheet.webp

安装位置：${CODEX_HOME:-$HOME/.codex}/pets/<id>/{pet.json,spritesheet.webp}
  install 直接安装成品包，不需要 Pillow/numpy/ffmpeg；build 默认构建后自动安装。

环境变量（可选覆盖，通常直接用 codex/opts.jsonc）：
  CODEX_HOME             安装根目录，默认 ~/.codex
  CODEX_VALIDATE_ATLAS   官方 validate_atlas.py 的路径；未设置则跳过官方校验
  CODEX_PET_CACHE        抽帧缓存目录，默认 <仓库>/.agents/tmp/codex-pet-frames
  CODEX_PET_OUT          产物目录，默认 <仓库>/.agents/tmp/codex-pet-out
  CODEX_PET_INSTALL      build 后是否自动安装（true/false），默认 true
  CODEX_PET_FORCE_EXTRACT 忽略抽帧缓存重新抽帧（true/false），默认 false

配置文件（codex/opts.jsonc，模板见 codex/opts.example.jsonc）：
  codexHome / validateAtlas / cacheDir / outDir / install / forceExtract
  优先级：命令行参数 > 环境变量 > 配置文件 > 内置默认值
  文件支持 // 与 /* */ 注释（JSONC）。

spec 字段：
  id / displayName / description     安装后的宠物标识与展示信息
  sourceDir                          动画目录（相对仓库根、相对 spec 目录或绝对路径）
  ground                             源帧里的地面线 y；"auto" 取每行所用帧的包围盒底边
  standardBase / lookBase            地面线落在单元格内的 y（默认 199 / 191）
  fitPadding                         scale="fit" 时四周留白（默认 4px）
  rows.<0-8>.animation               源动画名（<sourceDir>/<name>.webm 或 <sourceDir>/<name>/）
  rows.<0-8>.frames                  帧号列表（1 起）或 "even"（在整段里等距取样）
  rows.<0-8>.axis                    角色中轴在源帧里的 x；"auto" = alpha 加权重心
  rows.<0-8>.scale                   源帧→单元格的缩放；"fit" = 按包围盒自动适配
  rows.<0-8>.mirror                  左右镜像（可选）
  rows.<0-8>.ground / base           按行覆盖上面的默认值（可选）
  look.animation / frames / axis / scale / mirror   同上，frames 需要 16 帧

依赖（仅 build/inspect/validate 需要）：Pillow、numpy、ffmpeg（源为 webm 时）。
install 子命令只复制文件，不依赖这些包；系统 python 缺 Pillow 时改用任意
装了这两个包的 Python，例如 `pip install pillow numpy` 后的解释器。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

_imaging_error: ImportError | None = None
np = None
Image = None
try:
    import numpy as np
    from PIL import Image
except ImportError as exc:  # pragma: no cover
    _imaging_error = exc


def require_imaging() -> None:
    """install 只复制文件，不依赖 Pillow；构建与校验才强制要求。"""
    if _imaging_error is not None:
        raise SystemExit(
            "构建/校验需要 Pillow 与 numpy：pip install pillow numpy"
            "（或换用已装这两个包的 Python 解释器）"
        ) from _imaging_error


REPO = Path(__file__).resolve().parent.parent
DEFAULT_CACHE = REPO / ".agents" / "tmp" / "codex-pet-frames"
DEFAULT_OUT = REPO / ".agents" / "tmp" / "codex-pet-out"
DEFAULT_OPTS = Path(__file__).resolve().parent / "opts.jsonc"

# 配置文件字段（codex/opts.jsonc，模板 codex/opts.example.jsonc）。
# 优先级：命令行参数 > 环境变量 > 配置文件 > 内置默认值。
OPTION_ENV = {
    "codexHome": "CODEX_HOME",
    "validateAtlas": "CODEX_VALIDATE_ATLAS",
    "cacheDir": "CODEX_PET_CACHE",
    "outDir": "CODEX_PET_OUT",
    "install": "CODEX_PET_INSTALL",
    "forceExtract": "CODEX_PET_FORCE_EXTRACT",
}


def strip_jsonc(text: str) -> str:
    """去掉 JSONC 的 // 与 /* */ 注释（跳过字符串内部）。"""
    out: list[str] = []
    i, n = 0, len(text)
    in_string = False
    escaped = False
    while i < n:
        ch = text[i]
        if in_string:
            out.append(ch)
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
            i += 1
            continue
        if ch == '"':
            in_string = True
            out.append(ch)
            i += 1
            continue
        if text.startswith("//", i):
            while i < n and text[i] not in "\r\n":
                i += 1
            continue
        if text.startswith("/*", i):
            end = text.find("*/", i + 2)
            i = n if end == -1 else end + 2
            continue
        out.append(ch)
        i += 1
    return "".join(out)


def load_options(path: Path | str) -> dict:
    """读取可读配置文件（JSONC）；文件不存在时返回空表（全走环境变量/默认值）。"""
    path = Path(path).expanduser().resolve()
    if not path.exists():
        return {}
    try:
        raw = json.loads(strip_jsonc(path.read_text(encoding="utf-8")))
    except json.JSONDecodeError as exc:
        raise SystemExit(f"配置文件不是合法 JSON：{path}\n{exc}")
    if not isinstance(raw, dict):
        raise SystemExit(f"配置文件顶层必须是对象：{path}")
    unknown = set(raw) - set(OPTION_ENV)
    if unknown:
        raise SystemExit(f"配置文件有未知字段 {sorted(unknown)}：{path}")
    return raw


class Options:
    """把配置文件、环境变量与内置默认值合并成一份生效配置。"""

    def __init__(self, config: dict):
        self.config = config
        self.codex_home = self._path("codexHome", Path.home() / ".codex")
        self.validate_atlas = self._path("validateAtlas", None)
        self.cache_dir = self._path("cacheDir", DEFAULT_CACHE)
        self.out_dir = self._path("outDir", DEFAULT_OUT)
        self.install = self._flag("install", True)
        self.force_extract = self._flag("forceExtract", False)

    def _env(self, key: str) -> str | None:
        value = os.environ.get(OPTION_ENV[key])
        return value.strip() if value and value.strip() else None

    def _path(self, key: str, default: Path | None) -> Path | None:
        env = self._env(key)
        raw = env or self.config.get(key)
        if raw is None or (isinstance(raw, str) and not raw.strip()):
            return default
        return Path(os.path.expandvars(os.path.expanduser(str(raw))))

    def _flag(self, key: str, default: bool) -> bool:
        env = self._env(key)
        raw = env if env is not None else self.config.get(key)
        if raw is None:
            return default
        return self._parse_bool(raw, OPTION_ENV.get(key, key))

    @staticmethod
    def _parse_bool(value: object, label: str) -> bool:
        if isinstance(value, bool):
            return value
        text = str(value).strip().lower()
        if text in {"1", "true", "yes", "on"}:
            return True
        if text in {"0", "false", "no", "off"}:
            return False
        raise SystemExit(f"{label} 需要布尔值（true/false），收到 {value!r}")


def find_validator(options: Options) -> Path | None:
    path = options.validate_atlas
    return path if path is not None and path.is_file() else None


# 官方 v2 网格：8 列 × 11 行，单元格 192×208；行 0-8 标准动画，行 9-10 看向方向
CELL_W, CELL_H = 192, 208
COLS, ROWS = 8, 11
LOOK_COUNT = 16
REQUIRED_FRAMES = {0: 6, 1: 8, 2: 8, 3: 4, 4: 5, 5: 8, 6: 6, 7: 6, 8: 6}
ROW_STATES = {
    0: "idle",
    1: "running-right",
    2: "running-left",
    3: "waving",
    4: "jumping",
    5: "failed",
    6: "waiting",
    7: "running",
    8: "review",
}


def load_spec(path: Path) -> tuple[dict, Path]:
    spec_path = path.expanduser().resolve()
    try:
        spec = json.loads(spec_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise SystemExit(f"找不到 spec：{spec_path}")
    except json.JSONDecodeError as exc:
        raise SystemExit(f"spec 不是合法 JSON：{spec_path}\n{exc}")
    if not isinstance(spec, dict):
        raise SystemExit(f"spec 顶层必须是对象：{spec_path}")
    for key in ("id", "displayName", "rows", "look"):
        if not spec.get(key):
            raise SystemExit(f"spec 缺少字段 {key}：{spec_path}")
    return spec, spec_path.parent


def resolve_source_dir(spec: dict, spec_dir: Path, override: str | None, label: str) -> Path:
    raw = override or spec.get("sourceDir")
    if not raw:
        raise SystemExit(f"{label}: spec 缺少 sourceDir")
    cand = Path(os.path.expanduser(raw))
    candidates = [cand] if cand.is_absolute() else [spec_dir / cand, REPO / cand]
    for directory in candidates:
        if directory.is_dir():
            return directory.resolve()
    raise SystemExit(f"{label}: 找不到动画目录 {raw}（试过 {', '.join(map(str, candidates))}）")


def cache_key(name: str) -> str:
    return re.sub(r'[\\/:*?"<>|]', "_", name)


def frame_paths(source_dir: Path, name: str, cache_dir: Path, force: bool) -> list[Path]:
    """返回源动画的帧文件（按播放顺序）。webm 抽帧结果缓存在 cache_dir。"""
    webm = source_dir / f"{name}.webm"
    if webm.is_file():
        dst = cache_dir / cache_key(name)
        cached = sorted(dst.glob("*.png"))
        if cached and not force and cached[-1].stat().st_mtime >= webm.stat().st_mtime:
            return cached
        if shutil.which("ffmpeg") is None:
            raise SystemExit("抽帧需要 ffmpeg，但 PATH 里没有")
        dst.mkdir(parents=True, exist_ok=True)
        for old in dst.glob("*.png"):
            old.unlink()
        subprocess.run(
            [
                "ffmpeg", "-hide_banner", "-v", "error", "-c:v", "libvpx-vp9",
                "-i", str(webm), "-vsync", "0", "-start_number", "1",
                str(dst / "%03d.png"),
            ],
            check=True,
        )
        cached = sorted(dst.glob("*.png"))
        if not cached:
            raise SystemExit(f"{webm} 没有抽到任何帧")
        return cached

    png_dir = source_dir / name
    if png_dir.is_dir():
        files = sorted(png_dir.glob("*.png"))
        if files:
            return files

    raise SystemExit(f"找不到源动画 {name}：期望 {webm} 或目录 {png_dir}/")


def pick_frames(entry: dict, required: int, total: int, label: str) -> list[int]:
    frames = entry.get("frames", "even")
    if frames == "even":
        if total < required:
            raise SystemExit(f"{label}: 源动画只有 {total} 帧，不够取 {required} 帧")
        if required == 1:
            return [1]
        step = (total - 1) / (required - 1)
        picks = [round(1 + i * step) for i in range(required)]
        if len(set(picks)) != required:
            raise SystemExit(f"{label}: 等距取样出现重复帧 {picks}")
        return picks
    if not isinstance(frames, list) or len(frames) != required:
        raise SystemExit(
            f"{label}: frames 需要 {required} 个帧号（官方该行固定 {required} 帧）或 \"even\""
        )
    picks = []
    for value in frames:
        if not isinstance(value, int) or isinstance(value, bool):
            raise SystemExit(f"{label}: 帧号必须是整数，收到 {value!r}")
        if not 1 <= value <= total:
            raise SystemExit(f"{label}: 帧号 {value} 超出范围 1..{total}")
        picks.append(value)
    return picks


def load_frame(path: Path) -> Image.Image:
    img = Image.open(path).convert("RGBA")
    a = np.array(img)
    a[a[:, :, 3] < 8] = 0  # 清掉编码/抽帧留下的 alpha 边缘噪声
    a[a[:, :, 3] == 0, :3] = 0
    return Image.fromarray(a, "RGBA")


def analyze(images: list[Image.Image]) -> dict:
    """返回所有帧的联合包围盒（右/下为开区间）和 alpha 加权水平重心。"""
    min_x = min_y = max_x = max_y = None
    num = den = 0.0
    for img in images:
        alpha = np.asarray(img.getchannel("A"))
        ys, xs = np.nonzero(alpha)
        if xs.size == 0:
            continue
        weights = alpha[ys, xs].astype(np.float64)
        num += float((xs * weights).sum())
        den += float(weights.sum())
        x0, x1 = int(xs.min()), int(xs.max())
        y0, y1 = int(ys.min()), int(ys.max())
        min_x = x0 if min_x is None else min(min_x, x0)
        min_y = y0 if min_y is None else min(min_y, y0)
        max_x = x1 if max_x is None else max(max_x, x1)
        max_y = y1 if max_y is None else max(max_y, y1)
    if min_x is None or den == 0:
        raise SystemExit("选中的帧全透明，无法确定角色位置")
    return {"centroid": num / den, "bbox": (min_x, min_y, max_x + 1, max_y + 1)}


def plan_entry(
    entry: dict,
    *,
    required: int,
    default_base: int,
    label: str,
    spec: dict,
    spec_dir: Path,
    cache_dir: Path,
    force_extract: bool,
) -> dict:
    if not isinstance(entry, dict) or not entry.get("animation"):
        raise SystemExit(f"{label}: 缺少 animation")
    source_dir = resolve_source_dir(spec, spec_dir, entry.get("sourceDir"), label)
    paths = frame_paths(source_dir, entry["animation"], cache_dir, force_extract)
    picks = pick_frames(entry, required, len(paths), label)
    images = [load_frame(paths[number - 1]) for number in picks]
    stats = analyze(images)

    axis = entry.get("axis", spec.get("axis", "auto"))
    if axis == "auto":
        axis = stats["centroid"]
    elif not isinstance(axis, (int, float)) or isinstance(axis, bool):
        raise SystemExit(f"{label}: axis 需要数字或 \"auto\"")

    ground = entry.get("ground", spec.get("ground", "auto"))
    if ground == "auto":
        ground = float(stats["bbox"][3])
    elif not isinstance(ground, (int, float)) or isinstance(ground, bool):
        raise SystemExit(f"{label}: ground 需要数字或 \"auto\"")

    scale = entry.get("scale", spec.get("scale", "fit"))
    if scale == "fit":
        pad = entry.get("padding", spec.get("fitPadding", 4))
        width = stats["bbox"][2] - stats["bbox"][0]
        height = stats["bbox"][3] - stats["bbox"][1]
        scale = min((CELL_W - 2 * pad) / width, (CELL_H - 2 * pad) / height)
    elif not isinstance(scale, (int, float)) or isinstance(scale, bool) or scale <= 0:
        raise SystemExit(f"{label}: scale 需要正数或 \"fit\"")

    base = entry.get("base", default_base)
    if not isinstance(base, int) or isinstance(base, bool):
        raise SystemExit(f"{label}: base 需要整数")

    return {
        "label": label,
        "source": source_dir / entry["animation"],
        "total": len(paths),
        "picks": picks,
        "images": images,
        "axis": float(axis),
        "ground": float(ground),
        "scale": float(scale),
        "base": base,
        "mirror": bool(entry.get("mirror", False)),
    }


def prepare(
    spec: dict,
    spec_dir: Path,
    cache_dir: Path,
    force_extract: bool = False,
) -> tuple[dict[int, dict], dict]:
    standard_base = spec.get("standardBase", 199)
    look_base = spec.get("lookBase", 191)
    if not isinstance(standard_base, int) or not isinstance(look_base, int):
        raise SystemExit("standardBase / lookBase 需要整数")

    rows: dict[int, dict] = {}
    for row in range(9):
        entry = spec["rows"].get(str(row))
        if entry is None:
            raise SystemExit(f"spec.rows 缺少行 {row}（{ROW_STATES[row]}）")
        rows[row] = plan_entry(
            entry,
            required=REQUIRED_FRAMES[row],
            default_base=standard_base,
            label=f"row {row} {ROW_STATES[row]}",
            spec=spec,
            spec_dir=spec_dir,
            cache_dir=cache_dir,
            force_extract=force_extract,
        )

    look = plan_entry(
        spec["look"],
        required=LOOK_COUNT,
        default_base=look_base,
        label="look 16 方向",
        spec=spec,
        spec_dir=spec_dir,
        cache_dir=cache_dir,
        force_extract=force_extract,
    )
    return rows, look


def render_cell(src: Image.Image, axis: float, base: float, scale: float, ground: float, mirror: bool) -> Image.Image:
    """缩放源帧，使其地面线落在单元格 y=base、中轴落在单元格中心。"""
    sw = max(1, round(CELL_W / scale))
    sh = max(1, round(CELL_H / scale))
    x0 = round(axis - CELL_W / 2 / scale)
    y0 = round(ground - base / scale)
    padded = Image.new("RGBA", (sw + src.width, sh + src.height), (0, 0, 0, 0))
    padded.alpha_composite(src, dest=(max(0, -x0), max(0, -y0)))
    cell = padded.crop((max(0, x0), max(0, y0), max(0, x0) + sw, max(0, y0) + sh))
    cell = cell.resize((CELL_W, CELL_H), Image.Resampling.LANCZOS)
    if mirror:
        cell = cell.transpose(Image.Transpose.FLIP_LEFT_RIGHT)
    return cell


def clear_transparent_rgb(cell: Image.Image) -> Image.Image:
    a = np.array(cell)
    a[a[:, :, 3] == 0, :3] = 0
    return Image.fromarray(a, "RGBA")


def build(rows: dict[int, dict], look: dict, out_dir: Path) -> Path:
    atlas = Image.new("RGBA", (COLS * CELL_W, ROWS * CELL_H), (0, 0, 0, 0))

    for row, plan in rows.items():
        for col, image in enumerate(plan["images"]):
            cell = render_cell(
                image, plan["axis"], plan["base"], plan["scale"], plan["ground"], plan["mirror"]
            )
            atlas.alpha_composite(clear_transparent_rgb(cell), dest=(col * CELL_W, row * CELL_H))

    # v2 扩展中性帧：标准行里未用到的 (0, 6) 放一张待机首帧
    idle = rows[0]
    neutral = render_cell(
        idle["images"][0], idle["axis"], idle["base"], idle["scale"], idle["ground"], idle["mirror"]
    )
    atlas.alpha_composite(clear_transparent_rgb(neutral), dest=(6 * CELL_W, 0))

    for index, image in enumerate(look["images"]):
        cell = render_cell(
            image, look["axis"], look["base"], look["scale"], look["ground"], look["mirror"]
        )
        dest = ((index % COLS) * CELL_W, (9 + index // COLS) * CELL_H)
        atlas.alpha_composite(clear_transparent_rgb(cell), dest=dest)

    a = np.array(atlas)
    a[a[:, :, 3] == 0, :3] = 0
    atlas = Image.fromarray(a, "RGBA")

    out_dir.mkdir(parents=True, exist_ok=True)
    dst = out_dir / "spritesheet.webp"
    atlas.save(dst, "WEBP", lossless=True, quality=100, method=6, exact=True)
    return dst


def manifest_for_spec(spec: dict) -> dict:
    manifest = {"id": spec["id"], "displayName": spec["displayName"]}
    if spec.get("description"):
        manifest["description"] = spec["description"]
    manifest["spriteVersionNumber"] = 2
    manifest["spritesheetPath"] = "spritesheet.webp"
    return manifest


def write_package(spritesheet: Path, manifest: dict, codex_home: Path) -> Path:
    pet_id = manifest["id"]
    if not re.fullmatch(r"[A-Za-z0-9._-]+", str(pet_id)):
        raise SystemExit(f"宠物 id 含非法字符（只能用于目录名）：{pet_id!r}")
    pet_dir = codex_home / "pets" / str(pet_id)
    pet_dir.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(spritesheet, pet_dir / "spritesheet.webp")
    manifest = {**manifest, "spritesheetPath": "spritesheet.webp"}
    (pet_dir / "pet.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    return pet_dir


def install(spritesheet: Path, spec: dict, codex_home: Path) -> Path:
    return write_package(spritesheet, manifest_for_spec(spec), codex_home)


def install_package(package: Path, codex_home: Path) -> Path:
    """把已打包的目录（pet.json + 图集）原样安装到 codexHome。"""
    package = package.expanduser().resolve()
    manifest_path = package / "pet.json"
    if not manifest_path.is_file():
        raise SystemExit(f"找不到宠物清单：{manifest_path}")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise SystemExit(f"pet.json 不是合法 JSON：{manifest_path}\n{exc}")
    if not isinstance(manifest, dict) or not manifest.get("id"):
        raise SystemExit(f"pet.json 缺少 id：{manifest_path}")
    sprite_name = manifest.get("spritesheetPath", "spritesheet.webp")
    spritesheet = package / sprite_name
    if not spritesheet.is_file():
        raise SystemExit(f"找不到图集：{spritesheet}（pet.json 的 spritesheetPath）")
    return write_package(spritesheet, manifest, codex_home)


def validate(spritesheet: Path, options: Options) -> bool | None:
    validator = find_validator(options)
    if validator is None:
        print(
            "跳过官方校验：未找到 validate_atlas.py。"
            "可在 codex/opts.jsonc 里设置 validateAtlas 后重试。"
        )
        return None
    proc = subprocess.run(
        [sys.executable, str(validator), str(spritesheet), "--require-v2"],
        capture_output=True,
        text=True,
    )
    output = (proc.stdout or proc.stderr).strip()
    print(output)
    return proc.returncode == 0


def describe_plan(rows: dict[int, dict], look: dict) -> None:
    for row, plan in rows.items():
        print(
            f"row {row} {ROW_STATES[row]:<13} {len(plan['picks'])}/{REQUIRED_FRAMES[row]} frames"
            f"  picks={plan['picks']}"
            f"  axis={plan['axis']:.1f}  ground={plan['ground']:.1f}"
            f"  scale={plan['scale']:.3f}  base={plan['base']}"
            + ("  mirror" if plan["mirror"] else "")
        )
    print(
        f"look 16 方向       {len(look['picks'])}/{LOOK_COUNT} frames"
        f"  picks={look['picks']}"
        f"  axis={look['axis']:.1f}  ground={look['ground']:.1f}"
        f"  scale={look['scale']:.3f}  base={look['base']}"
        + ("  mirror" if look["mirror"] else "")
    )


def cmd_inspect(args: argparse.Namespace) -> int:
    require_imaging()
    options = Options(load_options(args.config))
    spec, spec_dir = load_spec(Path(args.spec))
    cache_dir = Path(args.cache).expanduser() if args.cache else options.cache_dir
    force_extract = args.force_extract or options.force_extract
    rows, look = prepare(spec, spec_dir, cache_dir, force_extract)
    describe_plan(rows, look)
    return 0


def cmd_build(args: argparse.Namespace) -> int:
    require_imaging()
    options = Options(load_options(args.config))
    spec, spec_dir = load_spec(Path(args.spec))
    cache_dir = Path(args.cache).expanduser() if args.cache else options.cache_dir
    out_dir = Path(args.out).expanduser() if args.out else options.out_dir / str(spec["id"])
    force_extract = args.force_extract or options.force_extract
    rows, look = prepare(spec, spec_dir, cache_dir, force_extract)
    describe_plan(rows, look)
    atlas = build(rows, look, out_dir)
    print(f"atlas: {atlas} ({atlas.stat().st_size} bytes)")

    if not args.skip_validate:
        ok = validate(atlas, options)
        if ok is False:
            raise SystemExit("官方校验未通过，未安装")
    if not args.no_install and options.install:
        print(f"installed: {install(atlas, spec, options.codex_home)}")
    else:
        print("未安装（--no-install 或配置 install=false）")
    return 0


def cmd_install(args: argparse.Namespace) -> int:
    options = Options(load_options(args.config))
    pet_dir = install_package(Path(args.package), options.codex_home)
    print(f"installed: {pet_dir}")
    return 0


def cmd_validate(args: argparse.Namespace) -> int:
    require_imaging()  # 官方校验器本身也依赖 Pillow，用同一个解释器运行
    options = Options(load_options(args.config))
    ok = validate(Path(args.atlas).expanduser(), options)
    return 0 if ok is not False else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="把 dsh-pet 透明动画打包成 Codex v2 宠物包",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__.split("用法：", 1)[1].split("spec 字段：", 1)[0],
    )
    sub = parser.add_subparsers(dest="command", required=True)

    def add_common(p: argparse.ArgumentParser) -> None:
        p.add_argument("spec", help="规格 JSON 路径")
        p.add_argument(
            "--config",
            default=str(DEFAULT_OPTS),
            help=f"配置文件路径（默认 {DEFAULT_OPTS}；模板 codex/opts.example.jsonc）",
        )
        p.add_argument("--cache", help="抽帧缓存目录（覆盖配置文件）")
        p.add_argument("--force-extract", action="store_true", help="忽略缓存重新抽帧")

    inspect = sub.add_parser("inspect", help="解析 spec 并打印每行的取帧与几何参数")
    add_common(inspect)
    inspect.set_defaults(func=cmd_inspect)

    build = sub.add_parser("build", help="构建 atlas、校验并安装")
    add_common(build)
    build.add_argument("--out", help="atlas 输出目录（覆盖配置文件）")
    build.add_argument("--no-install", action="store_true", help="只构建，不写入 codexHome")
    build.add_argument("--skip-validate", action="store_true", help="跳过官方校验脚本")
    build.set_defaults(func=cmd_build)

    install = sub.add_parser("install", help="把已打包的宠物目录安装到 codexHome（无需 Pillow）")
    install.add_argument(
        "package",
        nargs="?",
        default=str(Path(__file__).resolve().parent / "pets" / "whale-girl"),
        help="含 pet.json 与图集的目录（默认 codex/pets/whale-girl）",
    )
    install.add_argument(
        "--config",
        default=str(DEFAULT_OPTS),
        help=f"配置文件路径（默认 {DEFAULT_OPTS}）",
    )
    install.set_defaults(func=cmd_install)

    check = sub.add_parser("validate", help="用官方脚本校验已有 atlas")
    check.add_argument("atlas")
    check.add_argument(
        "--config",
        default=str(DEFAULT_OPTS),
        help=f"配置文件路径（默认 {DEFAULT_OPTS}）",
    )
    check.set_defaults(func=cmd_validate)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
