#!/usr/bin/env python3
"""Build a relocatable macOS app and a drag-to-Applications DMG."""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib

from fetch_macos_assets import prepare as prepare_mov_assets

ROOT = Path(__file__).resolve().parent.parent
APP_NAME = "Whale Pet.app"


def run(*args: str | Path, **kwargs) -> str:
    return subprocess.check_output([str(arg) for arg in args], text=True, **kwargs).strip()


def dependencies(path: Path) -> list[str]:
    # Dylibs list their own install name before their dependencies.
    ids = run("otool", "-D", path).splitlines()[1:]
    return [line.strip().split(" (compatibility version", 1)[0] for line in run("otool", "-L", path).splitlines()[1:]
            if line.strip().split(" (compatibility version", 1)[0] not in ids]


def is_system(name: str) -> bool:
    return name.startswith(("/System/Library/", "/usr/lib/"))


def bundle_libraries(executable: Path, frameworks: Path, ffmpeg: Path) -> list[Path]:
    queue = [executable]
    copied: dict[str, Path] = {}
    while queue:
        source = queue.pop()
        for name in dependencies(source):
            if is_system(name):
                continue
            dependency = Path(name)
            if not dependency.is_absolute() or not dependency.is_relative_to(ffmpeg):
                raise RuntimeError(f"Unexpected external dependency: {name}")
            if dependency.name not in copied:
                target = frameworks / dependency.name
                shutil.copy2(dependency, target)
                copied[dependency.name] = target
                queue.append(dependency)

    for binary in [executable, *copied.values()]:
        for name in dependencies(binary):
            if not is_system(name):
                prefix = "@executable_path/../Frameworks/" if binary == executable else "@loader_path/"
                run("install_name_tool", "-change", name, prefix + Path(name).name, binary)
        if binary != executable:
            run("install_name_tool", "-id", "@loader_path/" + binary.name, binary)
        for name in dependencies(binary):
            if not is_system(name) and not name.startswith(("@loader_path/", "@executable_path/../Frameworks/")):
                raise RuntimeError(f"Unbundled dependency in {binary.name}: {name}")
    return list(copied.values())


def license_files(folder: Path) -> list[Path]:
    prefixes = ("license", "licence", "copying", "notice", "copyright", "unlicense")
    return [p for p in folder.iterdir() if p.is_file() and p.name.lower().startswith(prefixes)]


def bundle_legal(resources: Path, target: str, ffmpeg: Path) -> None:
    legal = resources / "Legal"
    legal.mkdir()
    shutil.copyfile(ROOT / "release/LICENSE", legal / "LICENSE")
    shutil.copyfile(ROOT / "release/docs/THIRD_PARTY_NOTICES.md", legal / "THIRD_PARTY_NOTICES.md")
    shutil.copyfile(ROOT / "release/README.md", legal / "README.md")
    metadata = json.loads(run("cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target, cwd=ROOT))
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    pending = [next(p["id"] for p in metadata["packages"] if p["name"] == "whale-pet")]
    reached = set()
    while pending:
        name = pending.pop()
        if name in reached:
            continue
        reached.add(name)
        pending.extend(dep["pkg"] for dep in nodes[name]["deps"])

    fallback_root = ROOT / "release/licenses"
    fallbacks = {}
    for entry in json.loads((fallback_root / "index.json").read_text()):
        for package in entry["packages"]:
            fallbacks[package] = entry["files"]
    components = []
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if package["id"] not in reached:
            continue
        key = package["name"] + "-" + package["version"]
        folder = Path(package["manifest_path"]).parent
        files = license_files(folder)
        if package["source"] is None and key not in fallbacks:
            files = files or [ROOT / "LICENSE"]
        extra = fallbacks.get(key, []) if not files else []
        if not files and not extra:
            raise RuntimeError(f"Missing license text for {key}; update release/licenses")
        destination = legal / "Rust" / key
        destination.mkdir(parents=True)
        for source in files:
            shutil.copyfile(source, destination / source.name)
        for source in extra:
            path = fallback_root / source["file"]
            shutil.copyfile(path, destination / path.name)
        if package["name"] == "epaint_default_fonts":
            for source in (folder / "fonts").glob("*.txt"):
                shutil.copyfile(source, destination / source.name)
        components.append({"name": package["name"], "version": package["version"], "license": package["license"],
                           "repository": package["repository"], "license_sources": [item["url"] for item in extra]})
    (legal / "rust-components.json").write_text(json.dumps(components, indent=2, ensure_ascii=False) + "\n")

    recipe = (ROOT / "xtask/src/main.rs").read_text()
    constants = dict(re.findall(r'const (\w+): &str = "([^"]+)";', recipe))
    sources = legal / "Sources"
    sources.mkdir()
    decoder_info = {}
    for name, extension, folder_name, names in [
        ("FFMPEG", "tar.xz", "FFmpeg", ["LICENSE.md", "COPYING.LGPLv2.1"]),
        ("LIBVPX", "tar.gz", "libvpx", ["LICENSE", "PATENTS"]),
    ]:
        version = constants[name + "_VERSION"]
        stem = "ffmpeg" if name == "FFMPEG" else "libvpx"
        archive = ROOT / "third_party/ffmpeg/dl" / f"{stem}-{version}.{extension}"
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        if digest != constants[name + "_SHA256"]:
            raise RuntimeError(f"Source checksum mismatch: {archive.name}")
        shutil.copyfile(archive, sources / archive.name)
        original = ROOT / "third_party/ffmpeg/src" / f"{stem}-{version}{'-macos' if stem == 'libvpx' else ''}"
        destination = legal / folder_name
        destination.mkdir()
        for filename in names:
            shutil.copyfile(original / filename, destination / filename)
        decoder_info[stem] = {"version": version, "url": constants[name + "_URL"], "sha256": digest}
    shutil.copyfile(ROOT / "xtask/src/main.rs", sources / "xtask-main.rs")
    config = ROOT / "third_party/ffmpeg/build/ffmpeg-macos/ffbuild/config.mak"
    decoder_info["configuration"] = next(line.split("=", 1)[1].strip() for line in config.read_text().splitlines() if line.startswith("FFMPEG_CONFIGURATION="))
    decoder_info["configuration"] = decoder_info["configuration"].replace(str(ROOT), "<build-root>").replace(str(ffmpeg), "<ffmpeg-prefix>")
    (legal / "decoder-build.json").write_text(json.dumps(decoder_info, indent=2) + "\n")


def make_icon(resources: Path, staging: Path) -> None:
    iconset = staging / "WhalePet.iconset"
    iconset.mkdir()
    for size in [16, 32, 128, 256, 512]:
        for multiplier in [1, 2]:
            suffix = "@2x" if multiplier == 2 else ""
            run("sips", "-s", "format", "png", "-z", str(size * multiplier), str(size * multiplier), ROOT / "dsh-pet/assets/logo.png",
                "--out", iconset / f"icon_{size}x{size}{suffix}.png")
    run("iconutil", "-c", "icns", iconset, "-o", resources / "WhalePet.icns")


def main() -> None:
    if sys.platform != "darwin":
        raise SystemExit("This packager must run on macOS")
    os.chdir(ROOT)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    ffmpeg = Path(os.environ.get("FFMPEG_DIR", ROOT / "third_party/ffmpeg/out/macos")).resolve()
    env = {**os.environ, "FFMPEG_DIR": str(ffmpeg)}
    subprocess.run(["cargo", "build", "--locked", "--release", "-p", "whale-pet"], check=True, env=env)
    binary = ROOT / "target/release/whale-pet"
    arch = run("xcrun", "lipo", "-archs", binary)
    triples = {"arm64": "aarch64-apple-darwin", "x86_64": "x86_64-apple-darwin"}
    if arch not in triples:
        raise RuntimeError(f"Unsupported binary architecture: {arch}")
    name = f"whale-pet-{version}-macos-{arch}.dmg"
    output = ROOT / "release" / f"v{version}"
    output.mkdir(parents=True, exist_ok=True)
    temp_root = ROOT / ".agents/tmp"
    temp_root.mkdir(parents=True, exist_ok=True)
    with (temp_root / "macos-package.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with tempfile.TemporaryDirectory(prefix="macos-package-", dir=temp_root) as temporary:
            staging = Path(temporary)
            image_root = staging / "image"
            contents = image_root / APP_NAME / "Contents"
            macos, frameworks, resources = [contents / name for name in ["MacOS", "Frameworks", "Resources"]]
            for directory in [macos, frameworks, resources]:
                directory.mkdir(parents=True)
            executable = macos / "whale-pet"
            shutil.copy2(binary, executable)
            libraries = bundle_libraries(executable, frameworks, ffmpeg)
            for item in [executable, *libraries]:
                if run("xcrun", "lipo", "-archs", item) != arch:
                    raise RuntimeError(f"Architecture mismatch: {item.name}")
            minimum = max((re.search(r"minos ([\d.]+)", run("xcrun", "vtool", "-show-build", item)).group(1)
                           for item in [executable, *libraries]), key=lambda value: tuple(map(int, value.split("."))))
            assets = resources / "assets"
            assets.mkdir()
            shutil.copytree(prepare_mov_assets(), assets / "mov")
            for folder in ["webm", "fonts"]:
                shutil.copytree(ROOT / "dsh-pet/assets" / folder, assets / folder)
            for filename in ["config.jsonc", "anim-meta.json"]:
                shutil.copyfile(ROOT / "dsh-pet/assets" / filename, assets / filename)
            bundle_legal(resources, triples[arch], ffmpeg)
            make_icon(resources, staging)
            info = {"CFBundleName": "Whale Pet", "CFBundleDisplayName": "鲸鱼娘", "CFBundleExecutable": "whale-pet",
                    "CFBundleIdentifier": "io.github.kaleidox.whale-pet", "CFBundlePackageType": "APPL",
                    "CFBundleShortVersionString": version, "CFBundleVersion": version, "CFBundleIconFile": "WhalePet",
                    "CFBundleSupportedPlatforms": ["MacOSX"], "LSMinimumSystemVersion": minimum,
                    "LSUIElement": True, "NSHighResolutionCapable": True,
                    "NSHumanReadableCopyright": "Based on PC2005-cloud/dsh-pet — https://github.com/PC2005-cloud/dsh-pet"}
            with (contents / "Info.plist").open("wb") as file:
                plistlib.dump(info, file)
            for item in [*libraries, executable, contents.parent]:
                run("codesign", "--force", "--sign", "-", "--timestamp=none", item, stderr=subprocess.STDOUT)
            run("codesign", "--verify", "--deep", "--strict", contents.parent, stderr=subprocess.STDOUT)
            shutil.copyfile(ROOT / "release/README.md", image_root / "使用说明.md")
            (image_root / "Applications").symlink_to("/Applications", target_is_directory=True)
            archive = staging / name
            run("hdiutil", "create", "-quiet", "-fs", "HFS+", "-format", "UDZO", "-volname", f"Whale Pet {version}",
                "-srcfolder", image_root, archive)
            run("hdiutil", "verify", archive)
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            checksum = staging / (name + ".sha256")
            checksum.write_text(f"{digest}  {name}\n")
            os.replace(archive, output / name)
            os.replace(checksum, output / checksum.name)
            print(f"Created {output / name} ({(output / name).stat().st_size / 1024**2:.1f} MiB)")
            print(f"Minimum macOS: {minimum}; bundled dylibs: {len(libraries)}; SHA-256: {digest}")


if __name__ == "__main__":
    main()
