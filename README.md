**简体中文** | [English](README.en.md)

# Falcon Photo Viewer

面向摄影选片的桌面看图工具：快速浏览 RAW + JPEG 连拍、检查焦点、评分和比较，再将选中的照片交给后期软件。

[下载上游发行版](https://github.com/HWu0101/falcon-photo-viewer/releases) · [构建指南](BUILDING.md) · [设计与实现参考（英文）](docs/readme-reference.en.md)

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png)](https://slint.dev)

![Falcon：浏览连拍、查看 RAW 与 EXIF、检查 1:1 焦点、评分并导出](docs/images/falcon-workflow.png)

本仓库是 [HWu0101/falcon-photo-viewer](https://github.com/HWu0101/falcon-photo-viewer) 的 fork。原作由 **Hancheng Wu** 设计，并在 Claude 与 Codex 辅助下开发；原 README 中的摄影经历、设计过程和性能测量属于原作者，不代表本 fork 的独立实测。

## 功能特性

- **连拍浏览**：提前准备相邻照片预览，停止时显示全分辨率画面；实际速度取决于硬件与文件。
- **RAW + JPEG 配对**：将同一次拍摄作为一张照片管理，评分、标记、复制和删除同时作用于配对文件。
- **焦点与比较**：点击查看真实 1:1 像素，两张照片可同步缩放和平移。
- **选片与交接**：星级、选中/拒绝、撤销、原片复制，以及 sRGB JPG/PNG 导出。
- **文件夹工作流**：无需建立图库；支持色彩管理、沉浸模式和可选硬件解码加速。

## 快速开始

| 平台 | 要求与启动 |
| --- | --- |
| Windows | Windows 10/11 x64；解压发行包后运行 `Falcon.exe`；HEIC 需要 Microsoft HEIF Image Extensions |
| macOS | macOS 12+、Apple Silicon；将 `Falcon.app` 移至 Applications 后打开 |

发行包来自上游，包含程序、许可证和重建文档，应一并保留。Windows 包未签名；Mac 包为临时签名，未经 Apple 公证。先核对来源与校验值，再按系统提示处理首次启动；不要关闭系统安全检查。企业设备可能不允许例外，参见 [Apple 说明](https://support.apple.com/en-ie/102445)。校验值用于检查文件完整性，不等于发行者签名。

## 使用说明

打开照片所在文件夹，先浏览和评分，再比较关键帧并导出选片。

| 快捷键 | 操作 |
| --- | --- |
| `←` / `→` 或滚轮 | 上一张/下一张；按住浏览连拍 |
| 单击照片 | 指定位置 1:1 与适应窗口之间切换 |
| `1`–`5` / `0` | 评分/清除评分；再次按同一评分也可清除 |
| `P` / `X` / `U` | 选中/拒绝/取消标记 |
| `N` / `C` / `S` / `F` | 下一张未评分/双图比较/复查面板/沉浸模式 |
| `Ctrl+Z` / macOS `⌘Z` | 撤销，包含删除和移动 |

支持 JPEG、HEIC、常见相机 RAW、PNG/APNG、TIFF、WebP、JPEG XL、BMP 和 GIF。RAW 支持随相机型号而异，完整格式表见设计参考。它以浏览和选片为主，仅输出 SDR；没有完整照片编辑、跨文件夹图库、打印、上传或屏幕阅读器支持。

## 开发与验证

从仓库根目录进入 `falcon/`：

```bash
cd falcon
cargo build --locked --release --bin falcon
cargo test --locked --workspace
```

rustup 会安装仓库固定的 Rust 工具链。Windows 还需 Visual Studio C++ Build Tools 和 Windows SDK；Mac 需 Xcode。构建要求仓库内修改过的 winit 与 zune-jpeg。可选加速运行时单独检测，不附带 CUDA 或 HEVC 软件解码器。

私有相机测试素材不随源码发布，缺少时对应测试会明确跳过；测试总数不证明真实相机覆盖。完整环境及平台路径要求见 [BUILDING.md](BUILDING.md)。

## 文档与支持

- [原作者设计、功能与测量参考（英文）](docs/readme-reference.en.md) · [技术架构](logic.md)
- [重新构建](REBUILDING.md) · [参与贡献](CONTRIBUTING.md)
- [本 fork 问题反馈](https://github.com/DrivingGodJ/falcon-photo-viewer/issues)

## 许可与致谢

原始项目材料采用 [Apache-2.0](LICENSE)，版权属于 Hancheng Wu 及贡献者。依赖保留各自条款，见 [NOTICE](NOTICE) 与 [第三方声明](THIRD-PARTY-NOTICES.txt)。RAW 解码使用 LGPL-2.1 的 rawler，修改和重建方式见 [REBUILDING.md](REBUILDING.md)。Slint 徽章为所选工具包署名方式的一部分；软件部分基于 Independent JPEG Group 的工作。
