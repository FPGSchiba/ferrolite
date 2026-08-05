# Broken fixtures — RAWs FerroLite's decoder cannot handle

These files decode fine in LibRaw/exiftool but **fail through `rawler`**, the decoder
FerroLite actually uses. They live in this SIBLING directory, not under `fixtures/raw/`,
for three reasons:

1. `Catalog::ingest_folder` **recurses**, so even a `fixtures/raw/broken/` subdirectory
   gets swept into every ingest test — which is exactly what happened on the first
   attempt at this quarantine: `ingest_folder_indexes_images_and_thumbnails` and
   `second_ingest_skips_unchanged_files` both went red on `summary.failed == 0`.
2. `ferrolite-decode/tests/decode.rs::fixture()` selects the *first* RAW file `read_dir`
   returns from `fixtures/raw`. `read_dir` order is unspecified, so a panicking file
   in that tree is a latent way to detonate the whole decode suite.
3. They must not be swept into a hands-on ingest test and read as ordinary failures.

Per the v2 architecture map's fixed decision, **`rawler` is never forked** — gaps go
upstream (the Spec 4.6 precedent). These files are the reproductions for those reports.

The RAWs themselves are git-ignored. Re-download from the source URLs below if missing.

## Cases

### `nolens-canon-eos60d-iso100-50mm-mraw.CR2` — rawler **panics**

Canon EOS 60D **sRAW1 / mRAW**. This format stores YCbCr, not a CFA mosaic.

```text
assertion failed: self.initialized
  rawler-0.7.2/src/pixarray.rs:120
```

This is a **panic, not an `Err`**. Since all decode runs inside `ferrolite-jobs`
workers, an unguarded panic here takes out a worker thread on ingest — so any user
with a Canon sRAW file in their library hits it. `ferrolite-decode` now catches
unwinds at its public RAW entry points and reports `DecodeError::DecoderPanicked`,
which contains the blast radius, but the underlying rawler defect is unfixed.

**Partially decodable, so do not blanket-reject sRAW.** Only the paths that reach
rawler's CFA/`raw_image` handling panic — `read_metadata` and
`decode_meta_and_preview`. `decode_preview` **succeeds**, returning a 5184×3456 RGB8
image, because it extracts the embedded JPEG and never touches the mosaic. So the
thumbnail is perfectly usable; it is develop/full-decode that is not. All three
behaviours are pinned by `a_decoder_panic_becomes_an_error_not_an_unwind`.

- Source: <https://raw.pixls.us/getfile.php/994/nice/Canon%20-%20EOS%2060D%20-%20sRAW1%20(mRAW)%20(3%3A2).CR2>
- License: CC0 — freely usable in an upstream bug report.
- Upstream: see `docs/upstream/rawler-sraw-panic.md`.

### `orf-olympus-em1mk2-iso400-28mm.ORF` — `NoPreview`

Olympus E-M1 Mark II, 16-bit ORF. Returns `DecodeError::NoPreview` — a clean error,
no panic. rawler finds no embedded preview, full image, or thumbnail it can extract.

- Source: <https://raw.pixls.us/getfile.php/1993/nice/Olympus%20-%20E-M1MarkII%20-%2016bit%20(4%3A3).ORF>
- License: CC0.
- Upstream: see `docs/upstream/rawler-orf-nopreview.md`.

## Re-checking

Both are exercised by the fixture-gated tests in `ferrolite-decode/tests/decode.rs`
(they skip when the files are absent). When a rawler release fixes either, the
corresponding test starts failing on the "still broken" assertion — that is the signal
to move the file into `../raw/` and claim its slot in `../raw/FIXTURES.md`.
