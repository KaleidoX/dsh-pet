# Whale Pet（鲸鱼娘）

这是独立桌宠的 macOS Apple Silicon 测试安装包，自带动画、字体及解码库，无需安装 Rust、Node、Electron 或 FFmpeg。

打开 DMG，把 **Whale Pet.app** 拖到 **Applications**，再从“应用程序”打开。宠物出现在屏幕右上方。
请先退出已经运行的开发版桌宠，避免同时显示两只。

- 拖动人物移动位置，快速甩动后松手可抛出宠物。
- 点击人物会回应；右键打开动作菜单。
- 点击透明区域会传给下面的窗口。
- 点击人物使其获得焦点后，用 **Command+Q** 或 **Ctrl+Q** 退出。
- 应用不显示 Dock 图标；退出后删除应用即可卸载。

本包在 macOS 14.8.9、Apple Silicon、单屏 2× 缩放环境验证。最低系统版本由所带二进制的构建信息确定；其他系统尚未实测。
本机测试包使用 ad-hoc 签名，没有 Developer ID 签名或 Apple 公证。若系统首次打开时提示无法验证开发者，可在 Finder 中右键应用并选择“打开”。

代码采用 MIT 许可证，完整文本在应用包 `Contents/Resources/Legal/LICENSE`。
动画、图标与字体来自原项目，遵循其素材约定：允许开源使用，禁止商用。
原作者：<https://github.com/PC2005-cloud/dsh-pet>；衍生项目：<https://github.com/KaleidoX/dsh-pet>。
第三方组件声明、许可证和解码库源码随应用保存在 `Contents/Resources/Legal/`。
