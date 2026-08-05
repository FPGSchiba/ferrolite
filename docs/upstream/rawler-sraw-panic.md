# Upstream draft — rawler panics on Canon sRAW1/mRAW CR2

> **Status:** drafted 2026-08-05, **not yet filed**.
> **Target:** the `rawler` repository (issue tracker).
> **Why upstream:** the v2 architecture map fixes "`rawler` — never forked; missing
> cameras addressed upstream" (§2.1), with the Spec 4.6 camera-coverage work as the
> precedent. FerroLite carries a local *containment* (`DecodeError::DecoderPanicked`)
> but no local fix.
> **Repro file:** CC0, so it can be linked directly in the report.

---

## Title

`raw_image` panics (`assertion failed: self.initialized`) on Canon sRAW1/mRAW CR2

## Body

**rawler version:** 0.7.2
**Platform:** Windows 11, rustc 1.97.1 (also reproduces via the library API, not CLI-specific)

### Summary

Decoding a Canon **sRAW1 / mRAW** CR2 panics inside `pixarray.rs` instead of returning
an error. sRAW/mRAW files store downsampled **YCbCr** data rather than a CFA mosaic, so
there is no Bayer pattern to populate — but the code path still expects an initialized
pixel array.

Because a panic cannot be handled like a decode failure, any host application that
decodes user-supplied files in a worker pool loses the worker (or the process, under
`panic = "abort"`) on an otherwise ordinary unsupported-format case.

### Reproduction

```rust
let path = "Canon - EOS 60D - sRAW1 (mRAW) (3:2).CR2";
let mut src = rawler::RawSource::new(std::path::Path::new(path))?;
let decoder = rawler::get_decoder(&mut src)?;
let params = rawler::decoders::RawDecodeParams::default();

let _meta = decoder.raw_metadata(&mut src, &params)?;   // ok
let _dims = decoder.raw_image(&mut src, &params, true)?; // PANICS
```

### Actual

```text
thread '...' panicked at rawler-0.7.2/src/pixarray.rs:120:5:
assertion failed: self.initialized
```

Note it panics even with `dummy = true` (geometry-only, no pixel decode).

### Expected

`Err(RawlerError::Unsupported { .. })` — or full sRAW support if in scope. Either is
fine for a consumer; the panic is the problem.

### Sample file

<https://raw.pixls.us/getfile.php/994/nice/Canon%20-%20EOS%2060D%20-%20sRAW1%20(mRAW)%20(3%3A2).CR2>

From raw.pixls.us, licensed **CC0** — freely usable as a test fixture in-tree.
Canon EOS 60D, sRAW1, 3888×2592, 18 MB.

### Additional detail that may help triage

The **embedded-preview path works fine** on the same file — `preview_image` /
full-image extraction returns a valid 5184×3456 JPEG. Only the CFA/`raw_image` path
fails. So the container parses; it is specifically the mosaic assumption that breaks.

Related: Canon sRAW2 and the equivalent Nikon/Sony small-raw modes likely share the
shape of this bug; only sRAW1 was tested here.
