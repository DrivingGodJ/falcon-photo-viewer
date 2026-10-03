#!/usr/bin/env bash
#
# mac-bundle.sh — assemble a double-clickable Falcon.app from a built release binary
# and tar it (permissions preserved) for the mac-proto CI artifact.
#
# WHY A TARBALL. GitHub's upload-artifact strips POSIX exec bits from files it stores, so a
# bare binary (or an un-tarred .app) arrives non-executable and the tester has to chmod it.
# Tarring the finished bundle BEFORE upload preserves the 755 bit inside the artifact zip, so
# the tester's Terminal ritual (chmod / xattr / argv / tee) is retired: unpack, drag to
# /Applications, launch from Finder.
#
# NO PAID SIGNING (owner decision, 2026-10-01). The bundle remains ad-hoc
# (`-s -`) codesigned. Gatekeeper still flags it as un-notarized: the tester does ONE
# System Settings → Privacy & Security → Open Anyway after a blocked launch,
# after which macOS remembers the exception. Developer ID/notarization is not planned.
#
# USAGE:  scripts/mac-bundle.sh <release-binary-path> <output-dir>
# Runs on the macOS runner (sips / iconutil / plutil / codesign are preinstalled). The repo
# root is derived from this script's own location, so it does not depend on the caller's cwd.

set -euo pipefail

BIN="${1:?usage: mac-bundle.sh <release-binary-path> <output-dir>}"
OUT="${2:?usage: mac-bundle.sh <release-binary-path> <output-dir>}"
MODE="${3:-shipping}"
case "$MODE" in
    shipping) APP_NAME="Falcon"; ARCHIVE="" ;;
    candidate) APP_NAME="Falcon Mac Full 04"; ARCHIVE="falcon-1.0.8-mac-full04-candidate.tgz" ;;
    control) APP_NAME="Falcon Mac Control 04"; ARCHIVE="falcon-1.0.8-mac-full04-control.tgz" ;;
    native-host) APP_NAME="Falcon Mac Public Host 04"; ARCHIVE="falcon-1.0.8-mac-full04-public.tgz" ;;
    native-reference) APP_NAME="Falcon Mac Native Reference 04"; ARCHIVE="falcon-1.0.8-mac-full04-reference.tgz" ;;
    compat-host) APP_NAME="Falcon Mac Compat Host 04"; ARCHIVE="falcon-1.0.8-mac-full04-compat.tgz" ;;
    *) echo "mac-bundle: invalid mode: $MODE" >&2; exit 1 ;;
esac

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CARGO_TOML="$REPO_ROOT/falcon/native/Cargo.toml"
# Authoring produces full-size icon resources from the approved 1254px sources.
# Apple iconutil packages verified PNG iconsets; no imaging/font dependencies on the runner.
ICON_DIR="$REPO_ROOT/falcon/native/assets/icons"

# --- preconditions ------------------------------------------------------------------------
test -f "$BIN"        || { echo "mac-bundle: release binary not found: $BIN" >&2; exit 1; }
test -f "$CARGO_TOML" || { echo "mac-bundle: Cargo.toml not found: $CARGO_TOML" >&2; exit 1; }
python3 "$REPO_ROOT/scripts/check-icon-resources.py"
python3 "$REPO_ROOT/scripts/mac-experiment-bundle.py" --check-binary "$BIN" "$MODE"

# Parse the app version from Cargo.toml AT CI TIME (never hardcoded). The package `version`
# key is the only one anchored at column 0; dependency versions are indented or inlined.
VERSION="$(grep -m1 '^version[[:space:]]*=' "$CARGO_TOML" | sed -E 's/.*"([^"]+)".*/\1/')"
test -n "$VERSION" || { echo "mac-bundle: could not parse version from $CARGO_TOML" >&2; exit 1; }
# Apple bundle versions are numeric dotted fields, not Cargo SemVer prerelease strings.
# Keep the full Cargo version in custom metadata so an rc artifact is still identifiable.
if [[ "$VERSION" =~ ^([0-9]+\.[0-9]+\.[0-9]+)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    BUNDLE_VERSION="${BASH_REMATCH[1]}"
else
    echo "mac-bundle: invalid Cargo package version: $VERSION" >&2
    exit 1
fi
# A checkout must stamp its own HEAD. An archive supplies its published source revision.
SOURCE_ARGS=()
if [[ "$MODE" = shipping ]]; then SOURCE_ARGS+=(--require-clean); fi
SOURCE_REVISION="$(python3 "$REPO_ROOT/scripts/mac-experiment-bundle.py" --source-revision "$REPO_ROOT" ${SOURCE_ARGS[@]+"${SOURCE_ARGS[@]}"})"
if [[ "$MODE" = shipping ]]; then
    ARCHIVE="falcon-${VERSION}-macos-arm64.tgz"
fi
ARCHS="$(lipo -archs "$BIN")"
test "$ARCHS" = "arm64" || { echo "mac-bundle: expected native arm64 binary, got: $ARCHS" >&2; exit 1; }
echo "mac-bundle: building Falcon.app version $VERSION"

APP="$OUT/$APP_NAME.app"
CONTENTS="$APP/Contents"
mkdir -p "$OUT"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"

# --- executable ---------------------------------------------------------------------------
cp "$BIN" "$CONTENTS/MacOS/falcon"
chmod 755 "$CONTENTS/MacOS/falcon"

# --- Info.plist (numeric bundle version + complete Cargo/source provenance) --------------------------
cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>com.hwu0101.falcon</string>
	<key>CFBundleName</key>
	<string>Falcon Photo Viewer</string>
	<key>CFBundleDisplayName</key>
	<string>Falcon Photo Viewer</string>
	<key>CFBundleExecutable</key>
	<string>falcon</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleVersion</key>
	<string>$BUNDLE_VERSION</string>
	<key>CFBundleShortVersionString</key>
	<string>$BUNDLE_VERSION</string>
	<key>FalconSourceVersion</key>
	<string>$VERSION</string>
	<key>FalconSourceRevision</key>
	<string>$SOURCE_REVISION</string>
	<key>LSMinimumSystemVersion</key>
	<string>12.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>CFBundleIconFile</key>
	<string>falcon</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.photography</string>
	<!-- v0.9.16 (Round B assoc): the FULL viewer doc-type set — every Settings association family
	     (support.rs ASSOC_FAMILIES, minus the deliberately-unsupported avif/tga), system UTIs where
	     Apple declares one and UTImportedTypeDeclarations below where none exists. Role Viewer +
	     LSHandlerRank Alternate EVERYWHERE: declaring never claims the OS default — the in-app
	     "Make default" (LSSetDefaultRoleHandlerForContentType) is the only path that changes it.
	     The per-ext UTI evidence table lives in support.rs MAC_FAMILY_UTIS (pinned by the
	     mac_plist_covers_assoc_families test — edit BOTH together). -->
	<key>CFBundleDocumentTypes</key>
	<array>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Image</string>
			<key>CFBundleTypeIconFile</key>
			<string>falcon-document</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>public.jpeg</string>
				<string>public.png</string>
				<string>com.hwu0101.falcon.apng</string>
				<string>public.tiff</string>
				<string>public.heic</string>
				<string>public.heif</string>
				<string>org.webmproject.webp</string>
				<string>com.compuserve.gif</string>
				<string>com.microsoft.bmp</string>
				<string>public.jpeg-xl</string>
			</array>
		</dict>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Camera Raw Image</string>
			<key>CFBundleTypeIconFile</key>
			<string>falcon-document</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>com.canon.cr3-raw-image</string>
				<string>com.canon.cr2-raw-image</string>
				<string>com.canon.crw-raw-image</string>
				<string>com.nikon.raw-image</string>
				<string>com.nikon.nrw-raw-image</string>
				<string>com.sony.arw-raw-image</string>
				<string>com.sony.sr2-raw-image</string>
				<string>com.fuji.raw-image</string>
				<string>com.panasonic.rw2-raw-image</string>
				<string>com.adobe.raw-image</string>
				<string>com.olympus.or-raw-image</string>
				<string>com.olympus.raw-image</string>
				<string>com.pentax.raw-image</string>
				<string>com.samsung.raw-image</string>
				<string>com.epson.raw-image</string>
				<string>com.konicaminolta.raw-image</string>
				<string>com.hasselblad.3fr-raw-image</string>
				<string>com.phaseone.raw-image</string>
				<string>com.hwu0101.falcon.x3f</string>
			</array>
		</dict>
	</array>
	<!-- Types with NO system declaration (or one younger than LSMinimumSystemVersion 12.0):
	     - public.jpeg-xl IS Apple's identifier (UTType.jpegxl) but only since macOS 15.2 — importing
	       it makes .jxl resolve identically on 12.0–15.1; on >= 15.2 the system declaration wins.
	     - .apng / .x3f have no verified system or canonical vendor UTI, so they get identifiers in
	       OUR namespace (never public.* — that domain is Apple's). If a system declaration ever
	       appears for these extensions it wins automatically and the imports go inert. -->
	<key>UTImportedTypeDeclarations</key>
	<array>
		<dict>
			<key>UTTypeIdentifier</key>
			<string>public.jpeg-xl</string>
			<key>UTTypeDescription</key>
			<string>JPEG XL image</string>
			<key>UTTypeConformsTo</key>
			<array>
				<string>public.image</string>
			</array>
			<key>UTTypeTagSpecification</key>
			<dict>
				<key>public.filename-extension</key>
				<array>
					<string>jxl</string>
				</array>
			</dict>
		</dict>
		<dict>
			<key>UTTypeIdentifier</key>
			<string>com.hwu0101.falcon.apng</string>
			<key>UTTypeDescription</key>
			<string>Animated PNG image</string>
			<key>UTTypeConformsTo</key>
			<array>
				<string>public.png</string>
			</array>
			<key>UTTypeTagSpecification</key>
			<dict>
				<key>public.filename-extension</key>
				<array>
					<string>apng</string>
				</array>
			</dict>
		</dict>
		<dict>
			<key>UTTypeIdentifier</key>
			<string>com.hwu0101.falcon.x3f</string>
			<key>UTTypeDescription</key>
			<string>Sigma X3F raw image</string>
			<key>UTTypeConformsTo</key>
			<array>
				<string>public.camera-raw-image</string>
			</array>
			<key>UTTypeTagSpecification</key>
			<dict>
				<key>public.filename-extension</key>
				<array>
					<string>x3f</string>
				</array>
			</dict>
		</dict>
	</array>
</dict>
</plist>
PLIST

# Add provenance, tester instructions and notices before signing. Only deliberate
# diagnostic modes replace the regular identity and remove file associations.
python3 "$REPO_ROOT/scripts/mac-experiment-bundle.py" "$APP" "$MODE" "$SOURCE_REVISION"

# --- Apple packages the approved iconsets; verify the pixels stored in each representation ---
package_icon() {
    local source="$1" destination="$2"
    iconutil -c icns "$ICON_DIR/$source.iconset" -o "$CONTENTS/Resources/$destination.icns"
    python3 "$REPO_ROOT/scripts/check-icon-resources.py" --icns "$CONTENTS/Resources/$destination.icns" "$source"
}
package_icon app falcon
package_icon document falcon-document
python3 "$REPO_ROOT/scripts/check-icon-resources.py" "$APP"

# --- PkgInfo ------------------------------------------------------------------------------
printf 'APPL????' > "$CONTENTS/PkgInfo"

# --- ad-hoc seal (makes Gatekeeper's Open-Anyway flow behave consistently) ----------------
codesign --force --deep -s - "$APP"
codesign --verify --deep "$APP"

# --- SELF-VERIFY: hard-fail so CI never ships a broken bundle silently ---------------------
plutil -lint "$CONTENTS/Info.plist" >/dev/null
test -x "$CONTENTS/MacOS/falcon" || { echo "mac-bundle: executable bit missing" >&2; exit 1; }
ICNS="$CONTENTS/Resources/falcon.icns"
test -f "$ICNS" || { echo "mac-bundle: falcon.icns missing" >&2; exit 1; }
ICNS_SIZE="$(stat -f%z "$ICNS")"
test "$ICNS_SIZE" -gt 10240 || { echo "mac-bundle: falcon.icns too small ($ICNS_SIZE bytes)" >&2; exit 1; }
codesign --verify --deep "$APP" || { echo "mac-bundle: codesign verify failed" >&2; exit 1; }

# --- tar (exec bits preserved inside the artifact zip) ------------------------------------
PACKAGE_FILES=("$APP_NAME.app")
if [[ "$MODE" = shipping ]]; then
    PACKAGE_FILES+=(LICENSE NOTICE THIRD-PARTY-NOTICES.txt REBUILDING.md BUILDING.md docs/licenses "Read me.txt")
fi
tar --exclude='.falcon-generated-materials' -czf "$OUT/$ARCHIVE" -C "$OUT" "${PACKAGE_FILES[@]}"
echo "mac-bundle: wrote $OUT/$ARCHIVE (icns ${ICNS_SIZE} bytes, version $VERSION, mode $MODE)"
