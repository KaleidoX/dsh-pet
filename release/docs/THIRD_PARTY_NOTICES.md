# 第三方组件与素材

适用范围：Whale Pet 的 macOS 原生应用测试包。

原项目与素材作者：<https://github.com/PC2005-cloud/dsh-pet>。
动画、图标与字体沿用原仓库素材使用约定，允许开源使用，禁止商用；展示、介绍或分发衍生作品时须保留上述原作者链接。

应用动态链接 FFmpeg（裁剪构建，未启用 GPL、nonfree、version3），并在 avcodec 中静态链接 libvpx。
包内 `Legal/FFmpeg/` 和 `Legal/libvpx/` 保留原始许可文本，`Legal/Sources/` 提供本次构建使用的源码压缩包与构建脚本。
精确版本、来源、SHA-256 和构建配置记录在 `Legal/decoder-build.json`。

Rust 组件基于本次 Cargo.lock 和宿主平台的 Cargo 依赖图生成清单，包含运行代码及构建所需的组件。
精确组件名、版本、声明的许可证及来源见 `Legal/rust-components.json`，上游提供的许可和通知文本保存在 `Legal/Rust/`。
egui 内嵌字体的许可文本随 `epaint_default_fonts` 组件一并保留。

应用使用 macOS 系统框架，不复制或分发系统框架。
