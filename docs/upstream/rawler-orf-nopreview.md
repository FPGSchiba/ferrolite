# Upstream draft — no extractable preview from Olympus E-M1 Mark II ORF

> **Status:** drafted 2026-08-05, **not yet filed**.
> **Target:** the `rawler` repository (issue tracker).
> **Why upstream:** v2 architecture map §2.1 — `rawler` is never forked; gaps go
> upstream (Spec 4.6 precedent).
> **Severity:** low. Clean `Err`, no panic, no hang. Unlike the sRAW case
> (`rawler-sraw-panic.md`) this one degrades gracefully.
> **Repro file:** CC0.

---

## Title

No extractable preview/thumbnail from Olympus E-M1 Mark II ORF (16-bit)

## Body

**rawler version:** 0.7.2
**Platform:** Windows 11, rustc 1.97.1

### Summary

For this Olympus ORF, none of the preview routes yield an image: there is no
`preview_image` implementation, and neither the full embedded image nor the embedded
thumbnail can be extracted. A consumer that builds catalog thumbnails from the
embedded preview therefore cannot show this file at all, even though the file is
otherwise a normal, well-formed ORF.

Other formats tested from the same source (ARW, CR3, NEF, PEF, RAF, RW2) all return a
usable embedded image; ORF is the outlier in this sample set.

### Reproduction

```rust
let path = "Olympus - E-M1MarkII - 16bit (4:3).ORF";
let mut src = rawler::RawSource::new(std::path::Path::new(path))?;
let decoder = rawler::get_decoder(&mut src)?;
// full-image / thumbnail extraction yields nothing usable
```

### Actual

No preview obtained; the consuming application reports its own "no embedded preview,
full image, or thumbnail" error. No panic.

### Expected

An extractable embedded JPEG. Olympus ORFs normally carry one, so this may be a
matter of the relevant IFD/tag not being walked for this model or this bit depth.

### Sample file

<https://raw.pixls.us/getfile.php/1993/nice/Olympus%20-%20E-M1MarkII%20-%2016bit%20(4%3A3).ORF>

From raw.pixls.us, licensed **CC0**. Olympus E-M1 Mark II, 16-bit, 5240×3912, 17 MB.

### Note

Only metadata + preview extraction was exercised here — this report does **not**
claim the CFA decode path is affected. Worth checking whether the full raw decode
succeeds for the same file before triaging.
