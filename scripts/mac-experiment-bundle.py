"""Apply diagnostic identity/resources before mac-bundle.sh signs the app (stdlib only)."""
import os
import subprocess
import pathlib
import plistlib
import shutil
import sys

MODES = {"candidate": "Falcon Mac Full 04", "control": "Falcon Mac Control 04", "native-host": "Falcon Mac Public Host 04",
         "native-reference": "Falcon Mac Native Reference 04", "compat-host": "Falcon Mac Compat Host 04"}


def source_revision(root, override=None, require_clean=False):
    """A checkout owns its HEAD; an archive may supply its published source revision."""
    root = pathlib.Path(root).resolve()
    head = None
    try:
        top = subprocess.check_output(["git", "-C", str(root), "rev-parse", "--show-toplevel"], stderr=subprocess.DEVNULL).decode().strip()
        if pathlib.Path(top).resolve() == root:
            head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "--verify", "HEAD"], stderr=subprocess.DEVNULL).decode().strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        # An archive has no Git metadata/tool. It needs an explicit published revision.
        pass
    revision = override or head
    if not revision or len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
        raise ValueError("source archives require a full lowercase FALCON_SOURCE_REVISION")
    if head and revision != head:
        raise ValueError("FALCON_SOURCE_REVISION does not match this checkout's HEAD")
    if require_clean and head:
        dirty = subprocess.check_output(['git','-C',str(root),'status','--porcelain','--untracked-files=normal'])
        if dirty.strip(): raise ValueError('Shipping requires a clean checkout')
    return revision


def check_binary(binary, mode):
    if mode not in (*MODES, "shipping"):
        raise ValueError("unknown bundle mode")
    data = binary.read_bytes()
    has_feature = b"FALCON_MAC_CHROME_EXPERIMENT_" in data
    if has_feature != (mode != "shipping") or (mode != "shipping" and b"FALCON_MAC_CHROME_EXPERIMENT_04" not in data):
        raise ValueError("binary build feature does not match bundle mode")
    if mode == "shipping" and b"FALCON_MAC_NATIVE_TOOLBAR_01" not in data:
        raise ValueError("ordinary Mac binary is missing the tested native toolbar")


def diagnostic_plist(info, mode, revision):
    if mode not in MODES:
        raise ValueError("unknown experiment mode")
    if len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
        raise ValueError("expected full lowercase source revision")
    info = dict(info)
    for key in ("CFBundleDocumentTypes", "UTImportedTypeDeclarations", "UTExportedTypeDeclarations", "CFBundleURLTypes"):
        info.pop(key, None)
    version = info.get("FalconSourceVersion", info.get("FalconCargoVersion", info.get("CFBundleVersion", "unknown")))
    info.update(CFBundleIdentifier="com.hwu0101.falcon.mac-full-04." + mode,
                CFBundleName=MODES[mode], CFBundleDisplayName=MODES[mode],
                FalconExperimentMode=mode, FalconSourceRevision=revision,
                FalconBuildLabel=version + "-mac-full04",
                NSHumanReadableCopyright="Falcon Mac full04 — full-app candidate")
    return info


def configure(app, mode, revision):
    binary = app / "Contents/MacOS/falcon"
    check_binary(binary, mode)
    plist = app / "Contents/Info.plist"
    with plist.open("rb") as f:
        info = plistlib.load(f)
        if mode != "shipping":
            info = diagnostic_plist(info, mode, revision)
        elif len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
            raise ValueError("expected full lowercase source revision")
    with plist.open("wb") as f:
        plistlib.dump(info, f, sort_keys=False)
    resources = app / "Contents/Resources"
    resources.mkdir(parents=True, exist_ok=True)
    if mode != "shipping":
        (resources / "experiment-mode.txt").write_text(mode + "\n", encoding="utf-8")
    (resources / "source-revision.txt").write_text(revision + "\n", encoding="utf-8")
    guide = "release-tester" if mode == "shipping" else "full04-tester"
    shutil.copyfile(pathlib.Path(__file__).resolve().parents[1] / f"docs/platforms/macos/{guide}.md",
                    resources / "Tester instructions.txt")
    shutil.copyfile(pathlib.Path(__file__).resolve().parents[1] / f"docs/platforms/macos/{guide}.zh-cn.md",
                    resources / "Tester instructions zh-CN.txt")
    shutil.copyfile(pathlib.Path(__file__).resolve().parents[1] / "docs/third-party/chromium-chrome02.txt",
                    resources / "Chromium notice.txt")
    shutil.copyfile(pathlib.Path(__file__).resolve().parents[1] / "docs/third-party/winit-hosted-view.txt",
                    resources / "winit modifications.txt")
    shutil.copyfile(pathlib.Path(__file__).resolve().parents[1] / "falcon/vendor/winit/LICENSE",
                    resources / "winit LICENSE.txt")
    if mode == "shipping":
        from release_materials import copy as copy_materials, MATERIAL_MARKER
        copy_materials(pathlib.Path(__file__).resolve().parents[1], resources)
        # This app is freshly recreated by mac-bundle.sh; internal staging ownership is
        # unnecessary inside its signed resources. The reusable outer stage keeps its marker.
        (resources/'docs/licenses'/MATERIAL_MARKER).unlink()
        # Keep readable material next to the app as well as inside its signed resources.
        copy_materials(pathlib.Path(__file__).resolve().parents[1], app.parent)
        shutil.copyfile(resources/'Tester instructions.txt', app.parent/'Read me.txt')


if __name__ == "__main__":
    if sys.argv[1] == "--source-revision":
        print(source_revision(pathlib.Path(sys.argv[2]), os.environ.get("FALCON_SOURCE_REVISION"), '--require-clean' in sys.argv[3:]))
    elif sys.argv[1] == "--check-binary":
        check_binary(pathlib.Path(sys.argv[2]), sys.argv[3])
    else:
        configure(pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3])
