# Falcon changes to FemtoVG 0.25.1

Source: the published femtovg 0.25.1 crate, licensed under MIT OR Apache-2.0.
The original licence files and source notices are retained.

2026-10-04: keep cached wgpu scene pipelines across clear/target-only flushes.
Slint's rendering notifier performs a preparatory flush before drawing the scene.
The original per-flush cache eviction removes the scene pipelines at that point,
causing them to compile again on every frame. The patch executes/submits the clear
normally, and prunes/resets access bits only after actual scene drawing. It does
not retain photo textures, change shaders or alter rendering order.

Upstream report and maintainer confirmation:
https://github.com/slint-ui/slint/issues/12030

The production pruning helper has a CPU regression included in Falcon's native
test binary. This avoids upstream unit tests that require an example font absent
from the published crate. Metal execution remains a separate platform check.
