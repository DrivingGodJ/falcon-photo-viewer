# Production icons

Revision 3 uses the final alternative app master (approved 437:112), the larger A-series file labels/crop and the
unchanged generic-document v1 fallback approved by the owner.
`scripts/build-icons.py` composes the pinned source art and saved typography, with Inter SemiBold
from the bundled font. It generates the Windows ICO files and Mac PNG iconsets before building Falcon.
The app never generates icons or reads photos for icon selection. Public builds use these committed resources;
the private design handoff is not needed to compile or package them.

`approved-mask.png` is the unchanged generic-document 1024px alpha mask reconstructed from a mask-only Figma render with the
approved radius 256 and smoothing 0.6. The render's white fill and uniform #1e1e1e exterior were
converted to coverage; the artwork itself was not colour-keyed. Figma's SVG export omitted
smoothing, so it was not used. Main and document masters retain their charcoal interior.

The revision-2 recipe maps the approved Figma CROP transform to source UV coordinates. Text
uses Inter SemiBold, x=12/y=63, a 120px line box and base size 100; the nine approved sizes and
the approved 232px fit rule cover longer labels. HarfBuzz retains kerning. The app master
retains exact RGBA pixels; file renders are compared to independent native Figma references.
The file mask comes from the approved RAW master; generic-document pixels remain unchanged.
Icon alpha reduction uses area coverage to avoid faint ringing outside the mask.

Authoring dependencies: Pillow 12.3.0 and uharfbuzz 0.53.3. Run `python scripts/build-icons.py`
in the private artwork checkout with the revision-3 recipe available. These are developer tools only;
neither is a runtime/package dependency. Reproduction checks compare all output bytes. The
normal resource tests and Mac packaging need only Python's standard library.

`catalog.rs` pins Windows group resource IDs. Resource 1 and executable icon index 0 are always
the app. Resource 2 is the generic document fallback; 3 is generic RAW. Negative registry icon
references identify resources by ID. Never renumber existing IDs when adding formats.
`icon.ico` and `icon-256.png` in the native crate are generated app aliases for existing callers.

`app.iconset` and `document.iconset` contain PNG representations from 16 to 1024 pixels, including
Retina entries. Mac packaging uses Apple’s `iconutil` to encode compatible ICNS containers, then
decodes those containers and compares their displayed pixel coverage with the source frames. The Mac bundle declares the generic document resource for both existing
document groups; the app uses the final alternative master. These containers need native Finder/package acceptance before release.

Asset hashes are in `manifest.json`; `check-icon-resources.py` checks the actual bundle before
signing. `test-icon-resources.py` decodes its PNG entries independently and checks alpha, sizes and
the bundle script's actual declarations. Public source includes these complete resources; private design references are not required.
