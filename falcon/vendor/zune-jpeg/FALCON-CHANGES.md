# Falcon's zune-jpeg changes

Based on the published zune-jpeg 0.5.15 crate. Its original source, normalized Cargo manifest
and licence files are retained, except for the changes listed here. Falcon selects the upstream
Apache-2.0 option; the replacement is also Apache-2.0. `Cargo.toml.orig` is upstream history,
not the manifest used to build this copy.

`Cargo.toml` changes the licence field from the upstream alternatives to `Apache-2.0`,
the upstream option selected for this copy.

The AVX2 integer transpose in `src/unsafe_utils_avx2.rs` delegates to
`src/falcon_transpose.rs`. The replacement was written from the mathematical requirement
`output[row][column] = input[column][row]`; it preserves all 32-bit lane values. It does not
translate the former externally attributed snippet. Decoder selection, inverse-transform
arithmetic, output packing and the ARM/NEON implementation are unchanged.

`src/idct.rs` replaces the module's attribution comment with a description of the unchanged
inverse-transform implementations and a reference to this change list. This is a comment-only edit.

Tests compare all lane positions and arbitrary bit patterns with that requirement and check
the double transpose. Release preparation also compares JPEG output bytes and timing against
the untouched dependency. These tests establish behavior, not a legal certification.

A disabled example in `src/bitstream.rs` uses a generic test path instead of the upstream
developer's local profile path. This edit changes a comment only.

The complete original file inventory is in `scripts/zune-jpeg-upstream-sha256.json` at the
repository root. The accompanying patch check rejects undeclared changes or missing files.
