[简体中文](GUIDE.zh-CN.md) | **English**

# Usage, Configuration and Development

[Back to the project](../README.en.md)

## Quick start

| Platform | Requirements and launch |
| --- | --- |
| Windows | Windows 10/11 x64; extract the release and run `Falcon.exe`; HEIC needs Microsoft HEIF Image Extensions |
| macOS | macOS 12+ on Apple Silicon; move `Falcon.app` to Applications and open it |

Packages come from upstream and include licenses and rebuild documents; keep them with the app. Windows builds are unsigned; Mac builds have an ad-hoc signature without Apple notarization. Verify the source and checksums before following the system's first-launch prompts. Keep system security checks enabled; managed devices may disallow exceptions. See [Apple's guidance](https://support.apple.com/en-ie/102445). Checksums verify integrity, not publisher identity.

## Usage

Open a photo folder, browse and rate, compare key frames, then export your picks.

| Key | Action |
| --- | --- |
| `←` / `→` or wheel | Previous/next; hold to browse a burst |
| Click photo | Toggle 1:1 at that point and fit-to-window |
| `1`–`5` / `0` | Rate/clear; repeating a rating also clears it |
| `P` / `X` / `U` | Pick/reject/unmark |
| `N` / `C` / `S` / `F` | Next unrated/compare/review panel/immersive mode |
| `Ctrl+Z` / macOS `⌘Z` | Undo, including deletes and moves |

Supported formats include JPEG, HEIC, common camera RAW, PNG/APNG, TIFF, WebP, JPEG XL, BMP and GIF. RAW support varies by camera; the design reference has the full list. Falcon focuses on viewing and culling and outputs SDR only. It has no full photo editor, cross-folder catalogue, print/upload module or screen-reader support.

## Development and checks

From the repository root:

```bash
cd falcon
cargo build --locked --release --bin falcon
cargo test --locked --workspace
```

rustup installs the pinned toolchain. Windows also needs Visual Studio C++ Build Tools and the Windows SDK; Mac needs Xcode. The build uses the modified winit and zune-jpeg copies in this repository. Optional acceleration runtimes are detected separately; CUDA and HEVC software decoders are not bundled.

Private camera fixtures are not published. Tests explicitly skip those checks when fixtures are absent; totals do not prove real-camera coverage. See [BUILDING.md](../BUILDING.md) for full environment and platform-path requirements.

## Documentation and support

- [Original design, features and measurement reference](../docs/readme-reference.en.md) · [Technical architecture](../logic.md)
- [Rebuilding](../REBUILDING.md) · [Contributing](../CONTRIBUTING.md)
- [Report an issue in this fork](https://github.com/DrivingGodJ/falcon-photo-viewer/issues)

## License and credits

Original project material is [Apache-2.0](../LICENSE), copyright Hancheng Wu and contributors. Dependencies retain their own terms; see [NOTICE](../NOTICE) and [third-party notices](../THIRD-PARTY-NOTICES.txt). RAW decoding uses rawler under LGPL-2.1; [REBUILDING.md](../REBUILDING.md) explains modification and rebuilding. The Slint badge is part of the chosen toolkit attribution route. This software is based in part on the work of the Independent JPEG Group.
