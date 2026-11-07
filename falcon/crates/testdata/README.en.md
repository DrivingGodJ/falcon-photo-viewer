[简体中文](README.md) | **English**

# Optional Private Test Data

How Falcon loads optional camera fixtures and what they cover. The two NV12 camera-photo excerpts from the private development checkout are excluded from public source. Neither test nor production binaries embed them.

## Usage

Set the runtime variable `FALCON_PRIVATE_TILE_DIR` to a directory containing both files named in `REAL_TILE_FIXTURES`. An explicitly configured corpus that is missing, partial or malformed fails the check.

Do not substitute synthetic pixels under the same filenames: CPU/GPU tests pin the original hashes and expected color results. See [private fixture setup](../../../docs/testing/private-fixtures.md) for variables used by other optional camera corpora.

## Validation limits

Without a corpus, private rows print SKIP while synthetic golden cases, fuzz, layout and CPU/GPU arithmetic checks still run. Framework pass totals may include those reported skips and do not prove real-camera coverage.

Private photos are unnecessary for compilation or the portable suite. Byte-layout golden cases and source-photo checks are separate tests; synthetic coverage does not certify every camera model.
