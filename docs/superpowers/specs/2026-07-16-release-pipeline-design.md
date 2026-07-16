# FerroLite cross-platform release pipeline — design

**Date:** 2026-07-16
**Status:** approved (design), pending spec review
**Goal:** Produce sendable, installable FerroLite builds for Windows and macOS via a
repeatable GitHub Actions release pipeline: a Windows **NSIS `setup.exe`** and two
macOS **`.dmg`** installers (native Apple Silicon + native Intel). Fix the currently-red
CI as a prerequisite.

## Context

- `ferrolite-app` is an `eframe`/`egui` + `wgpu` desktop app with platform-specific code
  already present (`windows` crate on Windows, `core-graphics` on macOS), so it is
  genuinely cross-platform.
- The author develops on macOS. Cross-compiling this app from macOS to Windows is the
  fragile path (MSVC target needs a Windows linker; the mingw path must cross-build C
  deps like bundled SQLite, `ravif`, wgpu glue). The reliable, repeatable way to get a
  real Windows binary is to **build on Windows via GitHub Actions `windows-latest`**,
  which the existing CI already does. The same logic applies to the mac `.dmg`
  (`macos-latest`).
- CI (`.github/workflows/ci.yml`, matrix macOS + Windows) is **red on `main`** and has
  been for a while. The failure is not a real bug.

## Part 1 — Fix CI (prerequisite)

### The failure

The runner's current `stable` rustc turns a float-literal fallback future-compat lint
into a hard error:

```
error: falling back to `f32` as the trait bound `f32: From<f64>` is not satisfied
help: explicitly specify the type as `f32`: `1.5_f32`
```

It fires at **44 known sites, all in `ferrolite-app`** — egui calls where a bare float
literal (e.g. `Stroke::new(1.5, …)`, corner radii, thicknesses) is passed into an
`impl Into<f32>` parameter. Confirmed sites span:
`develop/crop_overlay.rs`, `develop/curve_widget_parametric.rs`,
`develop/histogram_widget.rs`, `develop/hsl_widget.rs`, `develop/mask_overlay.rs`,
`ingest.rs`, `library/develop_filter_bar.rs`, `library/develop_metadata_bar.rs`,
`library/filmstrip.rs`, `library/grid.rs`, `library/icons.rs`, `library/panel.rs`,
`widgets/color_wheel.rs`, `widgets/curve.rs`, `widgets/slider.rs`.

The app **builds clean on the author's local rustc 1.93.1** — only the newer runner
stable errors. The fix must therefore work on both.

### The fix

Add the `_f32` suffix the compiler already suggests at each site (e.g. `1.5` → `1.5_f32`).
This is forward-compatible and compiles on old and new stable alike, so **CI stays on
floating `stable`** (no toolchain pin).

Implementation note: `rustup update stable` locally to match the runner, then iterate
`cargo build --all-targets` until no fallback errors remain — the CI log may have
truncated past 44, so the local newer-stable build is the source of truth for the
complete list. Then run the full workspace gate:
`cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo build --all-targets`,
`cargo test --workspace`.

### Windows console suppression

Add to `ferrolite-app/src/main.rs`:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
```

So the shipped release GUI build does not spawn a console window on Windows. Debug builds
keep the console for logs.

## Part 2 — Packaging

### Tool: `cargo-packager`

Use [`cargo-packager`](https://github.com/crabnebula-dev/cargo-packager) (crabnebula):
one tool + one config block produces the macOS `.dmg` and the Windows NSIS `setup.exe`,
and auto-downloads NSIS on the runner (keeps the workflow thin). Rejected alternative:
`cargo-bundle` + `create-dmg` for mac and a hand-written `.nsi` + `makensis` for Windows
— two toolchains, two configs, more maintenance, chosen only if bespoke NSIS scripting is
needed (it is not).

### Config

Add `[package.metadata.packager]` to `ferrolite-app/Cargo.toml`:

- `product-name = "FerroLite"`
- `identifier = "dev.ferrolite.app"`
- Bundle **only** the `ferrolite-app` binary (exclude `bench_browse`).
- `category` = Photography.
- Per-OS `formats`: `dmg` on macOS, `nsis` on Windows.
- `icons` pointing at the committed icon set (below).

Exact cargo-packager field names/casing are confirmed against its docs during
implementation; the intent above is fixed.

### Icons

The installer/app icon must match the in-app icon, which is generated procedurally by
`ferrolite-app/src/chrome/icon.rs::icon_rgba(px)`. To avoid duplicating that geometry:

- Add a hidden `--export-icons <dir>` path to `main()` that renders `icon_rgba` at
  several sizes and writes `icon.png` (multi-size), `icon.ico`, and `icon.icns` into
  `<dir>` (using Rust encoders so no external tools are required; `iconutil`/ImageMagick
  are an acceptable fallback if a pure-Rust encoder is inconvenient).
- Run it once, commit the output under `ferrolite-app/packaging/icons/`.
- cargo-packager references those files. Regenerable anytime by re-running the flag.

## Part 3 — Release workflow (`.github/workflows/release.yml`)

New workflow, separate from `ci.yml` (which remains the PR/push gate).

### Triggers

- **`push` tag `v*`** → build all artifacts and publish a **GitHub Release** for the tag
  with the three artifacts attached.
- **`workflow_dispatch`** → same build, uploaded as downloadable **workflow artifacts**
  (dry-run without cutting a release).

### Matrix (3 artifacts)

| Job | Runner | Target | Output |
|-----|--------|--------|--------|
| win-x64 | `windows-latest` | `x86_64-pc-windows-msvc` | `FerroLite_<ver>_x64-setup.exe` (NSIS) |
| mac-arm | `macos-latest` | `aarch64-apple-darwin` | `FerroLite_<ver>_aarch64.dmg` |
| mac-x64 | `macos-latest` | `x86_64-apple-darwin` | `FerroLite_<ver>_x64.dmg` |

Two **separate** mac DMGs (native Apple Silicon + native Intel) — no universal binary,
per the decision that Apple Silicon is dropping Intel-binary support. The `x86_64` mac
target cross-compiles fine on `macos-latest` (same Apple toolchain, just add the target).

### Version stamping (tag drives version)

Before packaging, the workflow derives the version from the tag
(`${GITHUB_REF_NAME}` with a leading `v` stripped) and writes it into
`ferrolite-app/Cargo.toml` (`cargo set-version` from `cargo-edit`, or an equivalent
in-place edit) so the installer/DMG version always matches the tag. On
`workflow_dispatch` (no tag) the Cargo.toml version is used as-is.

### Steps (per job)

1. `actions/checkout@v4`
2. `dtolnay/rust-toolchain@stable` with the job's `target`
3. `Swatinem/rust-cache@v2`
4. (tag runs) stamp version from tag
5. `cargo install cargo-packager --locked`
6. `cargo packager --release --target <target>`
7. Collect the produced artifact
8. Publish: on tag → attach to the GitHub Release (`softprops/action-gh-release`);
   on dispatch → `actions/upload-artifact@v4`

## Signing — out of scope (documented)

Artifacts ship **unsigned / un-notarized**. Recipient UX:

- **Windows:** SmartScreen "unknown publisher" → *More info → Run anyway*.
- **macOS:** Gatekeeper "unidentified developer" → right-click → *Open*, or
  `xattr -dr com.apple.quarantine <app>`.

Acceptable for sending to a tester. Code signing + notarization can be added later behind
repo secrets without changing this pipeline's shape. This limitation is documented in the
repo (README or a short `docs` note) so recipients know the click-through.

## Verification

1. Workspace gate green locally on the updated stable, and CI green on macOS + Windows.
2. `workflow_dispatch` dry-run produces all three artifacts as downloadable files.
3. **Author hands-on test (per CLAUDE.md, load-bearing):** install
   `FerroLite_<ver>_x64-setup.exe` on the Windows PC and the matching `.dmg` on a Mac,
   launch, and confirm the app runs, the window/app icon is correct, and there is no
   stray console window on Windows.

## Out of scope

- Code signing / notarization (documented, deferrable).
- Linux packaging (`.deb`/AppImage) — cargo-packager supports it; add later if wanted.
- Auto-update / release-notes automation.
- Universal macOS binary (explicitly rejected in favor of split arch artifacts).
