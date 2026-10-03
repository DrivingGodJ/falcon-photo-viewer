# Optional private test data

The two NV12 files in the private development checkout contain decoded camera-photo excerpts.
They are excluded from the public source export. Tests load them at runtime; no test or production
binary embeds them. Do not substitute synthetic pixels under their names: their original hashes
and expected colour results remain pinned in the CPU/GPU tests.

Set `FALCON_PRIVATE_TILE_DIR` to a directory containing both named files in `REAL_TILE_FIXTURES`
to run the private rows. An explicitly supplied missing, partial or malformed corpus fails the
check. Without a corpus, tests print SKIP for the private rows and still run all synthetic golden
cases, fuzz, layout and CPU/GPU arithmetic checks. Test-framework pass totals can include these
reported skips; they are not proof of real-camera coverage.

Other optional camera corpora use the runtime variables documented in
[private fixture setup](../../../docs/testing/private-fixtures.md). No private photos are required
to compile or run the portable suite. The byte-layout golden rows and source-photo checks remain
separate tests; synthetic coverage does not certify every camera model.
