[简体中文](README.md) | **English**

# Production Icon Assets

Committed Windows/macOS icons for Falcon, with generation, resource-ID and packaging constraints. Public builds use these files without the private design handoff.

## Assets and constraints

Revision 3 uses the approved alternative app master (437:112), larger A-series file labels/crop and unchanged generic-document v1 fallback. `scripts/build-icons.py` composes pinned artwork and typography with bundled Inter SemiBold to produce Windows ICOs and Mac PNG iconsets. The running app never generates icons or reads photos for icon selection.

`approved-mask.png` is a 1024px generic-document alpha mask reconstructed from a mask-only Figma render, radius 256 and smoothing 0.6. White fill and uniform `#1e1e1e` exterior were converted to coverage; the artwork was not color-keyed. Figma's SVG export omitted smoothing and was not used. App and document masters retain charcoal interiors.

The revision-2 recipe maps the approved Figma CROP transform to source UV coordinates. Text uses Inter SemiBold, x=12/y=63, a 120px line box and base size 100. Nine approved sizes and the 232px fit rule cover longer labels; HarfBuzz retains kerning. The app master keeps exact RGBA pixels, and file renders are compared with independent native Figma references. The file mask comes from the approved RAW master; generic-document pixels stay unchanged. Alpha reduction uses area coverage to avoid edge ringing.

## Generation and usage

Reauthoring requires the private artwork checkout and revision-3 recipe, with Pillow 12.3.0 and uharfbuzz 0.53.3. Run `python scripts/build-icons.py` there. These are developer tools, not runtime/package dependencies. Normal resource tests and Mac packaging need only Python's standard library. Reproduction checks compare every output byte.

`catalog.rs` pins Windows icon-group resource IDs: 1 and EXE icon index 0 always identify the app, 2 is the generic document fallback and 3 is generic RAW. Negative registry references address resource IDs; never renumber existing IDs when adding formats. `icon.ico` and `icon-256.png` in the native crate are app aliases for existing callers.

## Validation and release

`app.iconset` and `document.iconset` contain 16–1024px PNGs, including Retina entries. Mac packaging uses Apple's `iconutil` to encode ICNS, then decodes and compares displayed pixel coverage. Both document groups use the generic document resource; the app uses the final alternative master. Native Finder/package acceptance is still required before release.

Asset hashes are in `manifest.json`. `check-icon-resources.py` checks the real bundle before signing. `test-icon-resources.py` independently decodes PNGs and checks alpha, sizes and bundle-script declarations. Public resources are complete; private design references are unnecessary.
