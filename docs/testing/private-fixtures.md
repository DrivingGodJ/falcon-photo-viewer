# Optional private fixture setup

Public source and its portable tests do not contain private camera photos. The source generates
synthetic inputs for format, pixel, orientation, cancellation and other regression checks. Some
additional integration checks need real-camera samples or platform codecs and hardware.

In the private development checkout, tracked Cargo configuration restores the owner's Windows
corpora automatically, including in detached review checkouts. Its `*_WINDOWS` defaults apply only
on Windows and `FALCON_REQUIRE_PRIVATE_FIXTURES=windows` makes missing, empty, unreadable or
cloud-only required inputs fail rather than silently skip. Native Mac/Linux tests ignore those
Windows defaults. The public exporter replaces the entire private Cargo config and rejects an
exported environment table or private guard; no private paths reach public source.

Set these portable variables at **test runtime** to override those defaults or opt into corpora in
a public checkout. They take precedence over Windows-only defaults:

| Variable | Input |
| --- | --- |
| `FALCON_PHOTO_TEST_DIR` | Mixed camera-photo folder used by the real-photo integration checks |
| `FALCON_HEIC_TESTKIT` | HEIC corpus |
| `FALCON_STANDARD_TESTKIT` | Standard multi-format corpus |
| `FALCON_EDGE_TESTKIT` | Edge-case format corpus |
| `FALCON_OTHER_RAW_TEST_DIR` | Additional RAW-camera samples |
| `FALCON_PRIVATE_TILE_DIR` | Both original NV12 excerpts named in `REAL_TILE_FIXTURES` |
| `FALCON_SELFIE_FIXTURE` | Optional exact HEIC orientation regression sample |
| `FALCON_NVJPEG_TEST_FILE` | Optional JPEG input for the nvJPEG probe example |

Set `FALCON_REQUIRE_PRIVATE_FIXTURES=1` to require coverage on any host. Use `0` only for an explicit
portable/no-corpus verification run, and report that choice. Codec/GPU unavailability remains a
separate reported skip. Never rename or move the real corpus for a guard test: override one path
to a nonexistent synthetic location in a fresh process instead.

When unconfigured, photo directories resolve beneath `falcon/crates/testdata/private/` in this
source tree. The standard-format decoder checks also recognize an existing local `Falcon/testkit/standard` folder in the current account's application-data directory. No personal drive or private checkout path is hard-coded. The optional
NV12 tests also recognize the original files in `falcon/crates/testdata/` in a private development
checkout; the export excludes them.

Run `cargo test --locked --workspace -- --nocapture` and retain the SKIP messages. A green summary
alone does not prove real-photo, codec or GPU coverage. NV12 synthetic golden rows always run;
real rows are reported as skipped without the private pixels. An explicitly configured missing,
incomplete or malformed NV12 corpus fails instead of silently skipping; supplied bytes retain
length, input-hash and output-golden checks. Do not replace real-camera bytes with arbitrary data
under existing names to get a passing result.

Existing fixture names and expected metadata in individual tests describe particular regressions.
Supplying an unrelated photo with the same filename does not make it a valid reference. Keep private
fixture provenance and the source photos out of published packages and source snapshots.
