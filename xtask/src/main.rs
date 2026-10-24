//! Build and run helper.
//!
//! `cargo xtask build-ffmpeg` downloads pinned FFmpeg and libvpx
//! sources, verifies their checksums and builds the trimmed decoder stack under
//! `third_party/ffmpeg/out`. Point `FFMPEG_DIR` at that directory when building
//! the app; `cargo xtask run` does it automatically. `cargo xtask anim-meta`
//! regenerates the pose metadata the animation switch reads.
//!
//! Only the pieces the pet needs are enabled: the libvpx VP9 decoder, the
//! Matroska demuxer, the VP9 parser, the file protocol and swscale. See
//! `.agents/findings.md` section 11 for measurements and the exact configure
//! output that was verified.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context as _, Result, bail};
use sha2::{Digest as _, Sha256};

const FFMPEG_VERSION: &str = "7.1.5";
const FFMPEG_URL: &str = "https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz";
const FFMPEG_SHA256: &str = "de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f";

const LIBVPX_VERSION: &str = "1.15.0";
const LIBVPX_URL: &str = "https://storage.googleapis.com/downloads.webmproject.org/releases/webm/libvpx-1.15.0.tar.gz";
const LIBVPX_SHA256: &str = "74360e13d4c42af1a9de82d99a4031437f44fe50b095abe9cd8431103d365f46";

const NASM_VERSION: &str = "2.16.03";
const NASM_URL: &str = "https://www.nasm.us/pub/nasm/releasebuilds/2.16.03/nasm-2.16.03.tar.xz";
const NASM_SHA256: &str = "1412a1c760bbd05db026b6c0d1657affd6631cd0a63cddb6f73cc6d4aa616148";

fn main() -> ExitCode {
    match main_impl() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xtask: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn main_impl() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("build-ffmpeg") => build_ffmpeg(args),
        Some("fetch-macos-assets") => fetch_macos_assets(),
        Some("run") => {
            let out = local_ffmpeg()?;
            let mut args = args.peekable();
            let debug = args.peek().is_some_and(|arg| arg == "--debug");
            if debug {
                args.next();
            }
            let mut command = vec!["run".to_owned(), "-p".to_owned(), "whale-pet".to_owned()];
            if !debug {
                command.push("--release".to_owned());
            }
            command.extend(args);
            let command = command.iter().map(String::as_str).collect::<Vec<_>>();
            if cfg!(target_os = "macos") {
                fetch_macos_assets()?;
                let status = Command::new("cargo")
                    .args(&command)
                    .env("FFMPEG_DIR", &out)
                    .env("WHALE_PET_MOV_DIR", workspace_root()?.join(".tools/macos-assets/mov"))
                    .status()?;
                if !status.success() {
                    bail!("cargo run failed ({status})");
                }
                Ok(())
            } else {
                cargo_with_ffmpeg(&out, &command)
            }
        }
        Some("check") => {
            cargo(&["fmt", "--all", "--check"])?;
            let out = local_ffmpeg()?;
            cargo_with_ffmpeg(&out, &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])?;
            cargo_with_ffmpeg(&out, &["test", "--workspace"])
        }
        Some("anim-meta") => {
            let out = local_ffmpeg()?;
            // Arguments after the command go straight to the generator.
            let mut command = vec!["run".to_owned(), "--release".to_owned(), "-p".to_owned(), "anim-meta".to_owned(), "--".to_owned()];
            command.extend(args);
            let command = command.iter().map(String::as_str).collect::<Vec<_>>();
            cargo_with_ffmpeg(&out, &command)
        }
        Some("package-macos") => {
            if std::env::consts::OS != "macos" {
                bail!("package-macos must run on macOS");
            }
            let status = Command::new("python3")
                .arg(workspace_root()?.join("scripts/package_macos.py"))
                .args(args)
                .env("FFMPEG_DIR", local_ffmpeg()?)
                .status()
                .context("run macOS packager")?;
            if !status.success() {
                bail!("macOS packaging failed ({status})");
            }
            Ok(())
        }
        Some("-h" | "--help") | None => {
            print!(
                "\
cargo xtask <COMMAND>

COMMANDS:
    build-ffmpeg [--target linux|windows|macos] [--jobs N]
                                                     Build the trimmed decoder stack (default: host)
    run [--debug] [-- OPTIONS]                        Build and run the pet with the local stack
    anim-meta [--asset-dir DIR]                      Regenerate assets/anim-meta.json (pose metadata)
    check                                            rustfmt --check + clippy + workspace tests
    fetch-macos-assets                               Fetch and verify upstream HEVC-alpha clips
    package-macos                                    Build a self-contained app and DMG
"
            );
            Ok(())
        }
        Some(other) => bail!("unknown command {other:?} (try --help)"),
    }
}

fn cargo(args: &[&str]) -> Result<()> {
    let status = Command::new("cargo").args(args).status().context("run cargo")?;
    if !status.success() {
        bail!("cargo {} failed ({status})", args.join(" "));
    }
    Ok(())
}

/// The host's decoder stack, or an explicitly supplied FFmpeg installation.
fn local_ffmpeg() -> Result<PathBuf> {
    let target = Target::host()?;
    let out = std::env::var_os("FFMPEG_DIR").map(PathBuf::from).unwrap_or(workspace_root()?.join("third_party/ffmpeg/out").join(target.name()));
    // libvpx is installed under its own prefix (`libvpx/`), so the FFmpeg
    // package config is what marks a finished build.
    if !out.join("lib/pkgconfig/libavcodec.pc").is_file() {
        bail!("{} is missing; run `cargo xtask build-ffmpeg --target {}` first (or set FFMPEG_DIR)", out.display(), target.name());
    }
    Ok(out)
}

fn cargo_with_ffmpeg(out: &Path, args: &[&str]) -> Result<()> {
    println!("  cargo {} (FFMPEG_DIR={})", args.join(" "), out.display());
    let status = Command::new("cargo").args(args).env("FFMPEG_DIR", out).status().context("run cargo")?;
    if !status.success() {
        bail!("cargo {} failed ({status})", args.join(" "));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Linux,
    Windows,
    Macos,
}

impl Target {
    fn host() -> Result<Self> {
        match std::env::consts::OS {
            "linux" => Ok(Self::Linux),
            "windows" => Ok(Self::Windows),
            "macos" => Ok(Self::Macos),
            other => bail!("unsupported host OS {other:?}"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Macos => "macos",
        }
    }
}

fn build_ffmpeg(mut args: impl Iterator<Item = String>) -> Result<()> {
    let mut target = Target::host()?;
    let mut jobs = std::thread::available_parallelism().map_or(4, |count| count.get());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--target" => {
                let value = args.next().context("--target needs linux, windows or macos")?;
                target = match value.as_str() {
                    "linux" => Target::Linux,
                    "windows" => Target::Windows,
                    "macos" => Target::Macos,
                    other => bail!("unknown --target {other:?} (expected linux, windows or macos)"),
                };
            }
            "--jobs" | "-j" => {
                jobs = args.next().context("--jobs needs a number")?.parse().context("--jobs must be a number")?;
                if jobs == 0 {
                    bail!("--jobs must be greater than zero");
                }
            }
            other => bail!("unknown argument {other:?}"),
        }
    }

    let root = workspace_root()?;
    let base = root.join("third_party/ffmpeg");
    let dl = base.join("dl");
    let src = base.join("src");
    let build = base.join("build");
    // Each target installs into its own prefix: a Windows install rewrites the
    // pkg-config files and drops .dll.a/.def files, which would confuse a
    // later Linux build pointed at the same directory.
    let out = base.join("out").join(target.name());
    for dir in [&dl, &src, &build, &out] {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }

    if target == Target::Macos && std::env::consts::OS != "macos" {
        bail!("the macOS decoder stack must be built on macOS");
    }
    let nasm = if target == Target::Macos && std::env::consts::ARCH == "aarch64" { PathBuf::new() } else { ensure_nasm(&dl, &src, &build)? };
    let ffmpeg_src = fetch(&dl, &src, "ffmpeg", FFMPEG_VERSION, FFMPEG_URL, FFMPEG_SHA256, "tar.xz", "")?;
    // libvpx refuses out-of-tree builds once a config.h exists, so each target
    // gets its own pristine source tree.
    let suffix = format!("-{}", target.name());
    let libvpx_src = fetch(&dl, &src, "libvpx", LIBVPX_VERSION, LIBVPX_URL, LIBVPX_SHA256, "tar.gz", &suffix)?;

    let build_dir = build.join(format!("ffmpeg-{}", target.name()));
    std::fs::create_dir_all(&build_dir).with_context(|| format!("create {}", build_dir.display()))?;
    match target {
        Target::Linux => build_linux(&libvpx_src, &ffmpeg_src, &build_dir, &out, jobs, &nasm)?,
        Target::Windows => build_windows(&libvpx_src, &ffmpeg_src, &build_dir, &out, jobs, &nasm)?,
        Target::Macos => build_macos(&libvpx_src, &ffmpeg_src, &build_dir, &out, jobs, &nasm)?,
    }

    println!("\nbuilt the {} stack in {}", target.name(), out.display());
    match target {
        Target::Linux | Target::Macos => {
            println!("run the app with: cargo xtask run");
            println!("or build with: FFMPEG_DIR={} cargo build -p whale-pet --release", out.display());
        }
        Target::Windows => {
            println!("build the app with: FFMPEG_DIR={} cargo build -p whale-pet --release --target x86_64-pc-windows-gnu", out.display())
        }
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest.parent().context("xtask is not inside the workspace")?.to_path_buf())
}

/// Downloads (once), verifies and extracts a source archive.
///
/// `suffix` distinguishes per-target source trees; the top directory inside the
/// archive (`<name>-<version>`) is renamed to `<name>-<version><suffix>`.
#[allow(clippy::too_many_arguments)]
fn fetch(dl: &Path, src: &Path, name: &str, version: &str, url: &str, sha256: &str, extension: &str, suffix: &str) -> Result<PathBuf> {
    let archive_name = format!("{name}-{version}");
    let dest = src.join(format!("{archive_name}{suffix}"));
    if dest.join("configure").is_file() {
        return Ok(dest);
    }

    let archive = dl.join(format!("{archive_name}.{extension}"));
    if !archive.is_file() {
        println!("downloading {url}");
        let status = Command::new("curl")
            .args(["--fail", "--location", "--silent", "--show-error", "--output"])
            .arg(&archive)
            .arg(url)
            .status()
            .context("run curl")?;
        if !status.success() {
            let _ = std::fs::remove_file(&archive);
            bail!("download failed ({status}): {url}");
        }
    }
    verify_sha256(&archive, sha256)?;

    println!("extracting {}", archive.display());
    let status = Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(src).status().context("run tar")?;
    if !status.success() {
        bail!("extract failed: {}", archive.display());
    }
    let extracted = src.join(&archive_name);
    if suffix.is_empty() {
        if !extracted.join("configure").is_file() {
            bail!("{} does not look like the {name} source tree", extracted.display());
        }
        return Ok(extracted);
    }
    if dest.exists() {
        std::fs::remove_dir_all(&dest).with_context(|| format!("remove stale {}", dest.display()))?;
    }
    std::fs::rename(&extracted, &dest).with_context(|| format!("rename into {}", dest.display()))?;
    Ok(dest)
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let digest = Sha256::digest(&data);
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if actual != expected {
        bail!("checksum mismatch for {}: expected {expected}, got {actual}", path.display());
    }
    Ok(())
}

/// nasm assembles FFmpeg's x86 SIMD. Prefer `$NASM`, then a copy built here.
fn ensure_nasm(dl: &Path, src: &Path, build: &Path) -> Result<PathBuf> {
    if let Some(nasm) = std::env::var_os("NASM").map(PathBuf::from) {
        return Ok(nasm);
    }
    let bin = build.join("tools/nasm/bin/nasm");
    if bin.is_file() {
        return Ok(bin);
    }
    if let Ok(path) = which("nasm") {
        return Ok(path);
    }

    let nasm_src = fetch(dl, src, "nasm", NASM_VERSION, NASM_URL, NASM_SHA256, "tar.xz", "")?;
    let prefix = build.join("tools/nasm");
    run(&nasm_src, Command::new("./configure").arg(format!("--prefix={}", prefix.display())), &[])?;
    run(&nasm_src, Command::new("make").arg("-j").arg(threads()), &[])?;
    run(&nasm_src, Command::new("make").arg("install"), &[])?;
    Ok(bin)
}

fn build_linux(libvpx: &Path, ffmpeg: &Path, build: &Path, out: &Path, jobs: usize, nasm: &Path) -> Result<()> {
    let tool_path = nasm.parent().unwrap_or(Path::new(""));
    println!("==> libvpx {LIBVPX_VERSION} (Linux)");
    let vpx_prefix = out.join("libvpx");
    run(
        libvpx,
        Command::new("./configure")
            .arg(format!("--prefix={}", vpx_prefix.display()))
            .arg("--target=x86_64-linux-gcc")
            .args([
                "--enable-shared",
                "--disable-static",
                "--disable-vp8",
                "--disable-vp9-encoder",
                "--disable-examples",
                "--disable-tools",
                "--disable-docs",
                "--disable-unit-tests",
                "--disable-decode-perf-tests",
                "--disable-encode-perf-tests",
            ])
            // Both configure scripts probe for nasm before ./configure returns.
            .env("PATH", prepend_path(tool_path)),
        &[],
    )?;
    make(libvpx, &[&format!("-j{jobs}")], tool_path)?;
    make(libvpx, &["install"], tool_path)?;

    println!("==> FFmpeg {FFMPEG_VERSION} (Linux)");
    run(
        build,
        Command::new(ffmpeg.join("configure"))
            .arg(format!("--prefix={}", out.display()))
            .args([
                "--disable-everything",
                "--enable-libvpx",
                "--enable-decoder=libvpx_vp9",
                "--enable-demuxer=matroska",
                "--enable-parser=vp9",
                "--enable-protocol=file",
                "--enable-swscale",
                "--enable-shared",
                "--disable-static",
                "--enable-pic",
                "--enable-pthreads",
                "--disable-gpl",
                "--disable-nonfree",
                "--disable-version3",
                "--disable-doc",
                "--disable-programs",
                "--disable-avdevice",
                "--disable-avfilter",
                "--disable-postproc",
                "--disable-network",
                "--disable-autodetect",
                "--disable-iconv",
                "--disable-sdl2",
                "--disable-xlib",
                "--disable-zlib",
                "--disable-bzlib",
                "--disable-lzma",
            ])
            .env("PKG_CONFIG_PATH", vpx_prefix.join("lib/pkgconfig"))
            // FFmpeg's configure also probes nasm for x86 SIMD.
            .env("PATH", prepend_path(tool_path)),
        &[],
    )?;
    make(build, &[&format!("-j{jobs}")], tool_path)?;
    make(build, &["install"], tool_path)
}

fn build_macos(libvpx: &Path, ffmpeg: &Path, build: &Path, out: &Path, jobs: usize, nasm: &Path) -> Result<()> {
    let tool_path = nasm.parent().unwrap_or(Path::new(""));
    let vpx_prefix = out.join("libvpx");
    println!("==> libvpx {LIBVPX_VERSION} (macOS, {})", std::env::consts::ARCH);
    run(
        libvpx,
        Command::new("./configure")
            .arg(format!("--prefix={}", vpx_prefix.display()))
            .args([
                "--disable-shared",
                "--enable-static",
                "--enable-pic",
                "--disable-vp8",
                "--disable-vp9-encoder",
                "--disable-examples",
                "--disable-tools",
                "--disable-docs",
                "--disable-unit-tests",
                "--disable-decode-perf-tests",
                "--disable-encode-perf-tests",
            ])
            .env("PATH", prepend_path(tool_path)),
        &[],
    )?;
    make(libvpx, &[&format!("-j{jobs}")], tool_path)?;
    make(libvpx, &["install"], tool_path)?;

    println!("==> FFmpeg {FFMPEG_VERSION} (macOS)");
    run(
        build,
        Command::new(ffmpeg.join("configure"))
            .arg(format!("--prefix={}", out.display()))
            .args([
                "--disable-everything",
                "--enable-libvpx",
                "--enable-decoder=libvpx_vp9",
                "--enable-demuxer=matroska",
                "--enable-parser=vp9",
                "--enable-protocol=file",
                "--enable-swscale",
                "--enable-shared",
                "--disable-static",
                "--enable-pic",
                "--enable-pthreads",
                "--disable-gpl",
                "--disable-nonfree",
                "--disable-version3",
                "--disable-doc",
                "--disable-programs",
                "--disable-avdevice",
                "--disable-avfilter",
                "--disable-postproc",
                "--disable-network",
                "--disable-autodetect",
                "--disable-iconv",
                "--disable-sdl2",
                "--disable-xlib",
                "--disable-zlib",
                "--disable-bzlib",
                "--disable-lzma",
            ])
            .env("PKG_CONFIG_PATH", vpx_prefix.join("lib/pkgconfig"))
            .env("PATH", prepend_path(tool_path)),
        &[],
    )?;
    make(build, &[&format!("-j{jobs}")], tool_path)?;
    make(build, &["install"], tool_path)
}

fn build_windows(libvpx: &Path, ffmpeg: &Path, build: &Path, out: &Path, jobs: usize, nasm: &Path) -> Result<()> {
    let prefix = std::env::var("MINGW_PREFIX").unwrap_or_else(|_| "x86_64-w64-mingw32-".to_owned());
    let gcc = format!("{prefix}gcc");
    which(&gcc)
        .with_context(|| format!("{gcc} not found in PATH; install mingw-w64 or set MINGW_PREFIX to the cross-prefix (e.g. x86_64-w64-mingw32-)"))?;

    // libvpx has no shared-library support on Windows, so it is linked
    // statically into avcodec.
    println!("==> libvpx {LIBVPX_VERSION} (Windows, static)");
    let vpx_prefix = out.join("libvpx");
    let tool_path = nasm.parent().unwrap_or(Path::new(""));
    run(
        libvpx,
        Command::new("./configure")
            .arg(format!("--prefix={}", vpx_prefix.display()))
            .arg("--target=x86_64-win64-gcc")
            .args([
                "--enable-static",
                "--disable-shared",
                "--disable-vp8",
                "--disable-vp9-encoder",
                "--disable-examples",
                "--disable-tools",
                "--disable-docs",
                "--disable-unit-tests",
                "--disable-decode-perf-tests",
                "--disable-encode-perf-tests",
            ])
            .env("CROSS", &prefix)
            // Both configure scripts probe for nasm before ./configure returns.
            .env("PATH", prepend_path(tool_path)),
        &[],
    )?;
    make(libvpx, &[&format!("-j{jobs}")], tool_path)?;
    make(libvpx, &["install"], tool_path)?;

    println!("==> FFmpeg {FFMPEG_VERSION} (Windows cross)");
    run(
        build,
        Command::new(ffmpeg.join("configure"))
            .arg(format!("--prefix={}", out.display()))
            .arg(format!("--cross-prefix={prefix}"))
            .args([
                "--target-os=mingw32",
                "--arch=x86_64",
                "--enable-cross-compile",
                "--disable-everything",
                "--enable-libvpx",
                "--enable-decoder=libvpx_vp9",
                "--enable-demuxer=matroska",
                "--enable-parser=vp9",
                "--enable-protocol=file",
                "--enable-swscale",
                "--enable-shared",
                "--disable-static",
                "--enable-pic",
                "--disable-gpl",
                "--disable-nonfree",
                "--disable-version3",
                "--disable-doc",
                "--disable-programs",
                "--disable-avdevice",
                "--disable-avfilter",
                "--disable-postproc",
                "--disable-network",
                "--disable-autodetect",
                "--disable-iconv",
                "--disable-sdl2",
                "--disable-xlib",
                "--disable-zlib",
                "--disable-bzlib",
                "--disable-lzma",
            ])
            // --enable-pthreads fails the cross probe; FFmpeg uses w32threads.
            .arg(format!("--extra-cflags=-I{}", vpx_prefix.join("include").display()))
            .arg(format!("--extra-ldflags=-L{}", vpx_prefix.join("lib").display()))
            .env("PKG_CONFIG_LIBDIR", vpx_prefix.join("lib/pkgconfig"))
            .env("PATH", prepend_path(tool_path)),
        &[],
    )?;
    make(build, &[&format!("-j{jobs}")], tool_path)?;
    make(build, &["install"], tool_path)
}

fn threads() -> String {
    std::thread::available_parallelism().map_or(4, |count| count.get()).to_string()
}

fn make(dir: &Path, args: &[&str], tool_path: &Path) -> Result<()> {
    let mut command = Command::new("make");
    command.args(args);
    if !tool_path.as_os_str().is_empty() {
        command.env("PATH", prepend_path(tool_path));
    }
    run(dir, &mut command, &[])
}

fn prepend_path(dir: &Path) -> std::ffi::OsString {
    let mut paths = vec![dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).unwrap_or_default()
}

fn which(name: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("{name} not found in PATH")
}

fn run(dir: &Path, command: &mut Command, extra_env: &[(&str, &Path)]) -> Result<()> {
    println!("  (cd {} && {command:?})", dir.display());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let status = command.current_dir(dir).stdin(Stdio::null()).status().with_context(|| format!("run {command:?}"))?;
    if !status.success() {
        bail!("command failed ({status}) in {}", dir.display());
    }
    Ok(())
}

fn fetch_macos_assets() -> Result<()> {
    let status = Command::new("python3").arg(workspace_root()?.join("scripts/fetch_macos_assets.py")).status()?;
    if !status.success() {
        bail!("MOV asset preparation failed ({status})");
    }
    Ok(())
}
