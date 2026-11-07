**简体中文** | [English](README.en.md)

# 生产图标资源

Falcon 已提交的 Windows 和 macOS 图标，以及生成、资源编号和打包检查约束。公开构建直接使用这些文件，不需要私有设计交付材料。

## 资源与约束

第 3 版使用已批准的替代应用主图（437:112）、较大的 A 系列文件标签/裁切，以及不变的通用文档 v1 后备图标。`scripts/build-icons.py` 用固定原图、排版和随附 Inter SemiBold 生成 Windows ICO 与 Mac PNG iconset。应用运行时不会生成图标或读取照片选择图标。

`approved-mask.png` 是 1024px 通用文档透明蒙版，来自仅含蒙版的 Figma 渲染，圆角 256、平滑度 0.6。白色填充与均匀 `#1e1e1e` 外部被转换为覆盖率，未对图稿做颜色抠除。Figma SVG 导出缺少平滑度，未采用；应用和文档主图保留深灰内部。

第 2 版配方将已批准的 Figma CROP 变换映射到源 UV 坐标。文字为 Inter SemiBold，x=12/y=63、行框 120px、基础字号 100；九种批准字号与 232px 适配规则处理较长标签，HarfBuzz 保留字距。应用主图保留精确 RGBA，文件图与独立的原生 Figma 参考比较。文件蒙版来自批准的 RAW 主图，通用文档像素不变；透明度缩减使用面积覆盖率，避免边缘振铃。

## 生成与使用

重新制作需要私有原图工程及第 3 版配方，开发工具依赖 Pillow 12.3.0 和 uharfbuzz 0.53.3。在该工程中运行 `python scripts/build-icons.py`。它们不是运行或发行依赖；常规资源测试和 Mac 打包仅需 Python 标准库。复现检查比较全部输出字节。

`catalog.rs` 固定 Windows 图标组资源 ID：1 和 EXE 索引 0 始终是应用图标，2 是通用文档，3 是通用 RAW。注册表的负数引用按资源 ID 定位；新增格式不能重编号。native crate 中的 `icon.ico` 与 `icon-256.png` 是供已有调用方使用的应用图标别名。

## 验证与发布

`app.iconset`、`document.iconset` 包含 16–1024px 及 Retina PNG。Mac 使用 Apple `iconutil` 编码 ICNS，再解码检查显示像素覆盖率。bundle 的两组文档类型均使用通用文档资源，应用使用最终替代主图；发布前仍需 Finder/原生安装包验收。

资源哈希在 `manifest.json`。`check-icon-resources.py` 在签名前检查真实 bundle；`test-icon-resources.py` 独立解码 PNG，检查透明度、尺寸与 bundle 脚本声明。公开资源完整，不依赖私人设计参考。
