# Camera Coverage

## Purpose & status

This is the v1 record of how ferrolite handles camera coverage for RAW
decoding, plus a prioritized backlog of hardening work. Coverage — which
camera models decode with a real color matrix versus which fall back to an
approximation — is not something ferrolite controls directly: the real
per-camera fix lives upstream in `rawler`, which is never forked. This
document exists so that gaps are tracked deliberately, contributed upstream
where possible, and revisited on a known cadence rather than discovered ad
hoc by users.

## Coverage today (as-built)

- RAW decode is fully delegated to `rawler = "0.7.2"` (crates.io, pinned;
  ~500+ camera models). There are no in-repo camera tables or per-camera
  overrides — ferrolite does not maintain its own coverage data.
- **Extension gate:** `RAW_EXTS` in `ferrolite-catalog/src/scan.rs` lists the
  26 RAW extensions the catalog will classify as `FileKind::Raw` and hand to
  `rawler`. Anything outside this list is never attempted as RAW.
- **Color path:** `ColorProfile::from_color_matrix` (in
  `ferrolite-decode/src/color.rs`) prefers the D65 illuminant from rawler's
  per-illuminant color matrices, falls back to any other present illuminant,
  and if neither is available falls back to `srgb_fallback()` (an
  sRGB-primaries, D65-white-point profile). This fallback is currently logged
  via `eprintln!`. The resulting profile is consumed by
  `ferrolite_color::camera_to_working` in `ferrolite-color/src/camera.rs` to
  map camera-space pixel data into the working color space.
- **Fixtures:** the test suite has one CC0 RAW sample,
  `fixtures/raw/sample.rw2` (Panasonic), so most cameras' decode paths are
  exercised only through `rawler`'s own test suite, not ferrolite's.
- **User-visible fallback (Spec 4.6):** previously the sRGB fallback was
  silent to the user. As of Spec 4.6, `ferrolite-app/src/develop/coverage.rs`
  derives a `CoverageStatus` (`NotApplicable` / `Pending` / `Calibrated` /
  `Fallback`) from the open image's kind, decode readiness, and the decoded
  profile's fallback flag via `camera_coverage(...)`. When the status is
  `Fallback`, the Develop camera-info row in
  `ferrolite-app/src/develop/adjustment_panel.rs` renders a warning chip
  ("approximate color") with a hover tooltip explaining that the camera has
  no known color profile and suggesting an upstream contribution.

## Deferred hardening backlog

| Item | Disposition | Notes |
|---|---|---|
| Camera-coverage audit tool | defer-to-v2 | Dev-tooling: walk a folder of RAW files and classify each as decoded-with-matrix / decoded-fallback / unsupported-by-rawler. This is the artifact used to file upstream contributions (samples + matrices) against `rawler`. |
| `RAW_EXTS` ↔ rawler consistency test | cheap-now | Guards that the classified-as-RAW extension set in `ferrolite-catalog/src/scan.rs` stays honest as `rawler`'s own supported-format list grows, so the catalog doesn't silently under- or over-classify files as RAW. |
| Structured fallback logging | cheap-now | Replace the current `eprintln!` in `ferrolite-decode/src/color.rs` with `tracing`, logged once per make/model rather than once per file, and including the make/model in the log fields for triage. |

## Upstream contribution workflow

`rawler` is never forked. When a camera model decodes without a usable color
matrix (or fails to decode at all), the fix is to contribute RAW samples and
color matrices upstream to `rawler`, not to add per-camera logic in this
repo. Coverage improvements land in ferrolite only when upstream `rawler`
cuts a release that includes the fix and this repo's `rawler` pin is bumped
to that release.

Before bumping the `rawler` pin, heed the project's recorded constraints:

- **MSRV floor** — the workspace pins `rust-version = "1.88"`; a `rawler`
  bump must not raise the effective minimum supported Rust version above
  this floor without a deliberate, separate decision.
- **`rusqlite` pin** — `rusqlite` is pinned at `0.32` (bundled feature) and
  must not be bumped incidentally; verify a `rawler` update does not drag in
  a newer `rusqlite` transitively.
- **CI runs `--all-features`** — CI builds and tests with all Cargo features
  enabled, so a default-off feature cannot be used to shield CI from a
  `rawler` bump's effects; any breakage surfaces immediately.

## Cadence (default, adjustable)

Opportunistic: bump `rawler` when an upstream release adds coverage for a
user-requested model, not on a fixed schedule. This is a default, not a
policy — it can be revisited if a backlog of pending upstream fixes builds
up faster than opportunistic bumps clear it.

## Camera-family prioritization (default, adjustable)

When choosing which upstream gaps to chase first, the default priority order
is: recent Canon CR3, Nikon Z (NEF), Sony (ARW), Fujifilm (RAF); then
whatever the camera-coverage audit tool (see backlog above) flags once it
exists. This is a default, not a policy — it can be reordered based on
actual user reports or audit findings.
