# Cross-Platform Release Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship installable FerroLite builds via a GitHub Actions release pipeline — a Windows NSIS `setup.exe` and two macOS `.dmg` installers (native Apple Silicon + native Intel) — after fixing the currently-red CI.

**Architecture:** Fix the float-literal lint that reddens CI, add Windows-GUI + icon-export prep to the app binary, describe packaging with `cargo-packager` metadata in `ferrolite-app/Cargo.toml`, generate installer icons from the app's own icon generator, and add a tag/dispatch-triggered `release.yml` that builds each artifact on its native runner and publishes them to a GitHub Release.

**Tech Stack:** Rust (stable), `eframe`/`egui`/`wgpu`, `cargo-packager`, NSIS (Windows), `hdiutil`/`iconutil` (macOS), GitHub Actions.

## Global Constraints

- Workspace gate MUST be green: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --all-targets`, `cargo test --workspace`.
- CI stays on floating `stable` toolchain — do NOT pin rustc to work around the lint.
- Never block the UI/update thread; never rebuild GPU pipelines per frame (repo CLAUDE.md rules) — not exercised by this plan, but do not violate.
- Product name: `FerroLite`. Bundle identifier: `dev.ferrolite.app`. Packaged binary: `ferrolite-app` ONLY (never `bench_browse`).
- macOS ships TWO separate DMGs (native `aarch64-apple-darwin` + native `x86_64-apple-darwin`) — no universal binary.
- Release version is TAG-DRIVEN: `vX.Y.Z` tag → stamp `X.Y.Z` into `ferrolite-app/Cargo.toml` before packaging.
- Artifacts are unsigned/un-notarized — documented, not solved.
- Conventional-commit messages. Commit after each task.

---

### Task 1: Fix the float-literal lint so CI is green on current stable

**Files:**
- Modify (add `_f32` suffix at flagged sites): all under `ferrolite-app/src/` —
  `develop/crop_overlay.rs`, `develop/curve_widget_parametric.rs`, `develop/histogram_widget.rs`,
  `develop/hsl_widget.rs`, `develop/mask_overlay.rs`, `ingest.rs`,
  `library/develop_filter_bar.rs`, `library/develop_metadata_bar.rs`, `library/filmstrip.rs`,
  `library/grid.rs`, `library/icons.rs`, `library/panel.rs`, `widgets/color_wheel.rs`,
  `widgets/curve.rs`, `widgets/slider.rs`.

**Interfaces:**
- Consumes: nothing.
- Produces: a green workspace gate on the current stable toolchain. No API changes.

**Background:** The runner's newer `stable` errors on bare float literals passed into egui's
`impl Into<f32>` params: `error: falling back to f32 as the trait bound f32: From<f64> is not satisfied`,
with a machine-applicable suggestion to suffix the literal (e.g. `1.5` → `1.5_f32`). The app builds
clean on older local stable, so the fix must be the suffix (works on both), NOT a toolchain pin.

- [ ] **Step 1: Match the runner's toolchain locally**

Run: `rustup update stable && rustc --version`
Expected: a stable newer than 1.93.1 becomes active.

- [ ] **Step 2: Reproduce the failure and capture the full site list**

Run: `cargo build --all-targets 2>&1 | grep -E "falling back|-->" | grep ferrolite | sed -E 's/^ *--> //' | sort -u`
Expected: FAIL — a list of `ferrolite-app/src/...rs:LINE:COL` sites (≈44; the exact set is authoritative here, the CI log may have truncated).

- [ ] **Step 3: Apply the suggested `_f32` suffix at every flagged site**

For each `file:line:col`, open the file and add `_f32` to the float literal the compiler points at
(the error's `help:` line gives the exact replacement, e.g. `1.0` → `1.0_f32`, `1.5` → `1.5_f32`,
`2.0` → `2.0_f32`). Change ONLY the flagged literal; do not touch surrounding code.

- [ ] **Step 4: Rebuild until no fallback errors remain**

Run: `cargo build --all-targets 2>&1 | grep -c "falling back"`
Expected: `0`. If non-zero, repeat Step 3 for the newly reported sites.

- [ ] **Step 5: Run the full workspace gate**

Run:
```bash
cargo fmt --all -- --check \
&& cargo clippy --workspace --all-targets -- -D warnings \
&& cargo build --all-targets \
&& cargo test --workspace
```
Expected: all four succeed (exit 0).

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix(app): suffix f32 literals to satisfy newer-stable float fallback lint"
```

---

### Task 2: App release-prep — hide Windows console + add `--export-icons`

**Files:**
- Modify: `ferrolite-app/src/main.rs` (top attribute + arg handling in `main`)
- Modify: `ferrolite-app/Cargo.toml` (add `"ico"` to the `image` feature list)

**Interfaces:**
- Consumes: existing `chrome::icon::icon_rgba(px: u32) -> Vec<u8>` (RGBA8, `px`×`px`).
- Produces: `ferrolite-app --export-icons <dir>` writes, into `<dir>`:
  - `icon.png` (512×512),
  - `icon.ico` (multi-size, from 16/32/48/64/128/256),
  - `AppIcon.iconset/` containing the Apple-named PNGs (`icon_16x16.png`, `icon_16x16@2x.png`,
    `icon_32x32.png`, `icon_32x32@2x.png`, `icon_128x128.png`, `icon_128x128@2x.png`,
    `icon_256x256.png`, `icon_256x256@2x.png`, `icon_512x512.png`, `icon_512x512@2x.png`).
  Then returns `Ok(())` WITHOUT launching the GUI.

- [ ] **Step 1: Add the Windows-subsystem attribute**

Add as the VERY FIRST line of `ferrolite-app/src/main.rs` (before the `mod` list):
```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
```
(Release Windows builds run without a console; debug builds keep it for logs.)

- [ ] **Step 2: Enable ICO encoding in the `image` dep**

In `ferrolite-app/Cargo.toml`, change the `image` line's features from
`["jpeg", "png", "tiff", "webp"]` to `["jpeg", "png", "tiff", "webp", "ico"]`.

- [ ] **Step 3: Add the `--export-icons` branch to `main`**

At the very start of `fn main()` (before `diag::init()`), insert:
```rust
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--export-icons") {
        let dir = args.get(pos + 1).cloned().unwrap_or_else(|| "packaging/icons".to_string());
        export_icons(std::path::Path::new(&dir)).expect("icon export failed");
        return Ok(());
    }
```
Then add this free function in `main.rs` (uses `image`, already a dep):
```rust
fn export_icons(dir: &std::path::Path) -> std::io::Result<()> {
    use image::{ImageBuffer, Rgba};
    let iconset = dir.join("AppIcon.iconset");
    std::fs::create_dir_all(&iconset)?;

    let render = |px: u32| -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        let rgba = chrome::icon::icon_rgba(px);
        ImageBuffer::from_raw(px, px, rgba).expect("icon_rgba size mismatch")
    };

    // Main PNG.
    render(512).save(dir.join("icon.png")).map_err(std::io::Error::other)?;

    // ICO: image 0.25's IcoEncoder writes a single-image ICO; encode a 256px master
    // (the ICO format max) which Windows/NSIS accept and downscale for smaller slots.
    {
        use image::codecs::ico::IcoEncoder;
        let mut ico = std::fs::File::create(dir.join("icon.ico"))?;
        IcoEncoder::new(&mut ico)
            .encode_image(&image::DynamicImage::ImageRgba8(render(256)))
            .map_err(std::io::Error::other)?;
    }

    // Apple .iconset PNGs.
    let apple: [(u32, &str); 10] = [
        (16, "icon_16x16.png"), (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"), (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"), (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"), (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"), (1024, "icon_512x512@2x.png"),
    ];
    for (px, name) in apple {
        render(px).save(iconset.join(name)).map_err(std::io::Error::other)?;
    }
    Ok(())
}
```

- [ ] **Step 4: Verify it builds and runs the export path**

Run: `cargo run -p ferrolite-app -- --export-icons /tmp/ferro-icons && ls -R /tmp/ferro-icons`
Expected: exits WITHOUT opening the GUI; `/tmp/ferro-icons/icon.png`, `/tmp/ferro-icons/icon.ico`, and `/tmp/ferro-icons/AppIcon.iconset/` with 10 PNGs all exist.

- [ ] **Step 5: Verify the gate still passes**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo build --all-targets`
Expected: exit 0. (Fix any clippy nit in `export_icons`, e.g. needless closures, until clean.)

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/main.rs ferrolite-app/Cargo.toml
git commit -m "feat(app): windows_subsystem release attr + --export-icons subcommand"
```

---

### Task 3: Generate icon assets and add cargo-packager metadata

**Files:**
- Create: `ferrolite-app/packaging/icons/icon.png`, `icon.ico`, `icon.icns` (committed assets)
- Modify: `ferrolite-app/Cargo.toml` (add `[package.metadata.packager]`)
- Reference (do not commit): `.gitignore` — ensure `dist/` (cargo-packager output) is ignored

**Interfaces:**
- Consumes: `ferrolite-app --export-icons` from Task 2.
- Produces: committed platform icons + packager config such that
  `cargo packager --release` produces a `.dmg` (mac) / NSIS `.exe` (Windows).

- [ ] **Step 1: Generate the raw icon assets into the committed folder**

Run: `cargo run -p ferrolite-app -- --export-icons ferrolite-app/packaging/icons`
Expected: `ferrolite-app/packaging/icons/{icon.png,icon.ico,AppIcon.iconset/}` created.

- [ ] **Step 2: Build the `.icns` from the iconset (macOS-native tool)**

Run:
```bash
iconutil -c icns ferrolite-app/packaging/icons/AppIcon.iconset \
  -o ferrolite-app/packaging/icons/icon.icns
rm -rf ferrolite-app/packaging/icons/AppIcon.iconset
```
Expected: `ferrolite-app/packaging/icons/icon.icns` exists; the intermediate iconset is removed (only `icon.png`, `icon.ico`, `icon.icns` remain).

- [ ] **Step 3: Add the packager metadata**

Append to `ferrolite-app/Cargo.toml`:
```toml
[package.metadata.packager]
product-name = "FerroLite"
identifier = "dev.ferrolite.app"
category = "Photography"
icons = ["packaging/icons/icon.icns", "packaging/icons/icon.ico", "packaging/icons/icon.png"]
before-packaging-command = "cargo build --release --bin ferrolite-app"
binaries = [{ path = "ferrolite-app", main = true }]

[package.metadata.packager.macos]
minimum-system-version = "11.0"

[package.metadata.packager.nsis]
# NSIS installer defaults (per-user install, Start-menu shortcut); auto-downloaded by cargo-packager.
```
Confirm exact field names/casing against `cargo packager --help` and the cargo-packager
docs during this step; adjust if the tool reports unknown keys. The intent (product name,
identifier, single `ferrolite-app` binary, icon set, dmg+nsis) is fixed.

- [ ] **Step 4: Ignore the packager output directory**

Ensure `ferrolite-app/dist/` (or root `dist/`) is git-ignored. If not already covered, add
`dist/` to `.gitignore`.

- [ ] **Step 5: Smoke-test a real local package (macOS arm64 dmg)**

Run:
```bash
cargo install cargo-packager --locked
cd ferrolite-app && cargo packager --release --formats dmg
```
Expected: a `FerroLite_*_*.dmg` appears under the cargo-packager output dir (`target/` or `dist/`).
If cargo-packager rejects a metadata key, fix Step 3 and re-run. (This validates icons + config
before wiring CI. Building release deps may take several minutes.)

- [ ] **Step 6: Verify the gate (metadata must not break the workspace)**

Run: `cargo build --all-targets && cargo fmt --all -- --check`
Expected: exit 0.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/packaging/icons/icon.png ferrolite-app/packaging/icons/icon.ico ferrolite-app/packaging/icons/icon.icns ferrolite-app/Cargo.toml .gitignore
git commit -m "feat(release): app icons + cargo-packager metadata (dmg/nsis)"
```

---

### Task 4: Release workflow (`.github/workflows/release.yml`)

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: cargo-packager metadata (Task 3), tag-driven version.
- Produces: on `v*` tag → a GitHub Release with 3 assets
  (`*_x64-setup.exe`, `*_aarch64.dmg`, `*_x64.dmg`); on `workflow_dispatch` → the same
  files as downloadable workflow artifacts.

**CRITICAL target/path alignment:** Task 3's `before-packaging-command` is
`cargo build --release --bin ferrolite-app`, which builds to the HOST release dir
(`target/release/`). But each CI job packages a specific `--target <triple>`, and
cargo-packager then looks for the binary (and writes its output) under
`target/<triple>/release/`. If the two disagree, packaging fails ("binary not found") — this
bites the mac-x64-on-arm64 cross job hardest. The fix: set `CARGO_BUILD_TARGET: <triple>` in
the **Package step's env only** (NOT globally — a global value would make the earlier
`cargo install cargo-packager`/`cargo-edit` steps try to build those tools for the wrong
target). With `CARGO_BUILD_TARGET` set, the baked-in `before-packaging-command` builds into
`target/<triple>/release/`, matching where cargo-packager `--target` looks and writes. So
Task 3's local smoke test (no `--target`) landed the dmg in `target/release/`; the CI jobs
(with `--target`) land it in `target/<triple>/release/` — that is where the collect step must
glob.

- [ ] **Step 1: Locally validate the cross-target mechanism (do this FIRST)**

This arm64 Mac can reproduce the exact mac-x64 CI job. Run:
```bash
rustup target add x86_64-apple-darwin
cd ferrolite-app
CARGO_BUILD_TARGET=x86_64-apple-darwin cargo packager --release --formats dmg --target x86_64-apple-darwin --verbose
ls -la ../target/x86_64-apple-darwin/release/*.dmg
cd ..
```
Expected: a `FerroLite_0.0.1_x64.dmg` (or similar `*_x64.dmg`) appears in
`target/x86_64-apple-darwin/release/`. This confirms BOTH that the `CARGO_BUILD_TARGET`
mechanism produces the binary where cargo-packager expects it AND the exact output directory
the workflow must collect from. Note the confirmed path and the exact dmg filename for Step 2.
(This is a heavy first cross release build — use the max timeout; cargo resumes incrementally.)

- [ ] **Step 2: Write the workflow**

Create `.github/workflows/release.yml` (the collect step globs the path confirmed in Step 1):
```yaml
name: release
on:
  push:
    tags: ["v*"]
  workflow_dispatch:

permissions:
  contents: write

jobs:
  package:
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: windows-latest
            target: x86_64-pc-windows-msvc
            formats: nsis
          - os: macos-latest
            target: aarch64-apple-darwin
            formats: dmg
          - os: macos-latest
            target: x86_64-apple-darwin
            formats: dmg
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{ matrix.target }}
      - name: Stamp version from tag
        if: startsWith(github.ref, 'refs/tags/v')
        shell: bash
        run: |
          VER="${GITHUB_REF_NAME#v}"
          cargo install cargo-edit --locked
          cargo set-version --package ferrolite-app "$VER"
      - name: Install cargo-packager
        run: cargo install cargo-packager --locked
      - name: Package
        working-directory: ferrolite-app
        env:
          CARGO_BUILD_TARGET: ${{ matrix.target }}
        run: cargo packager --release --formats ${{ matrix.formats }} --target ${{ matrix.target }} --verbose
      - name: Collect artifacts
        shell: bash
        run: |
          mkdir -p out
          find "target/${{ matrix.target }}/release" -maxdepth 1 \( -name '*.dmg' -o -name '*setup.exe' \) -exec cp {} out/ \; 2>/dev/null || true
          # Fallback: default release dir, in case CARGO_BUILD_TARGET routing differs.
          find "target/release" -maxdepth 1 \( -name '*.dmg' -o -name '*setup.exe' \) -exec cp {} out/ \; 2>/dev/null || true
          ls -la out
          test -n "$(ls -A out 2>/dev/null)" || { echo "no artifacts collected"; exit 1; }
      - name: Upload workflow artifacts (dispatch)
        if: github.event_name == 'workflow_dispatch'
        uses: actions/upload-artifact@v4
        with:
          name: ferrolite-${{ matrix.target }}
          path: out/*
      - name: Attach to release (tag)
        if: startsWith(github.ref, 'refs/tags/v')
        uses: softprops/action-gh-release@v2
        with:
          files: out/*
```
Note: the collect step runs from the repo root (no `working-directory`), so `target/...` is
the workspace target dir where cargo-packager wrote (confirmed in Step 1). It fails the job if
nothing was collected, so a path regression surfaces loudly instead of publishing an empty
release.

- [ ] **Step 3: Validate the workflow syntax**

Run (if `actionlint` is available): `actionlint .github/workflows/release.yml`
Expected: no errors. If `actionlint` is not installed, validate YAML instead:
`python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml')); print('ok')"`
Expected: `ok`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): tag/dispatch workflow building win nsis + mac dmgs"
```

- [ ] **Step 5: Note the remaining runtime-only risk for review**

The Windows NSIS job cannot be validated locally on this Mac — its target/path mechanism is
identical to the mac jobs (validated in Step 1), but the actual NSIS `setup.exe` output name
is only confirmable on the first `windows-latest` run. During the first `workflow_dispatch`
run, check the Windows job's `Collect artifacts` log: if it fails with "no artifacts
collected", read the `--verbose` packaging output for the real output path/name and adjust the
`find`. This is expected first-run verification, not a defect.

---

### Task 5: Document the unsigned-install click-through

**Files:**
- Modify: `README.md` (add a "Download & Install" section)

**Interfaces:**
- Consumes: nothing.
- Produces: user-facing install instructions covering the unsigned-artifact warnings.

- [ ] **Step 1: Add the install section**

Append to `README.md`:
```markdown
## Download & Install

Installers are published on the [Releases](https://github.com/FPGSchiba/ferrolite/releases) page:

- **Windows:** `FerroLite_<version>_x64-setup.exe` (NSIS installer)
- **macOS (Apple Silicon):** `FerroLite_<version>_aarch64.dmg`
- **macOS (Intel):** `FerroLite_<version>_x64.dmg`

Builds are currently **unsigned**, so the OS shows a one-time warning:

- **Windows:** SmartScreen "Windows protected your PC" → **More info** → **Run anyway**.
- **macOS:** "unidentified developer" → right-click the app → **Open** (or run
  `xattr -dr com.apple.quarantine /Applications/FerroLite.app`).

To cut a release, push a tag: `git tag v0.1.0 && git push origin v0.1.0`.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: download & install instructions for unsigned release artifacts"
```

---

## Self-Review

**Spec coverage:**
- Part 1 CI fix → Task 1 (f32 suffixes) + Task 2 Step 1 (windows_subsystem). ✓
- Part 2 packaging: cargo-packager metadata → Task 3; icons from `icon_rgba` → Task 2/3. ✓
- Part 3 workflow: triggers, 3-artifact matrix, version stamping, publish → Task 4. ✓
- Signing out-of-scope but documented → Task 5. ✓
- Verification (gate green, dispatch dry-run, author hands-on) → gate in Tasks 1–3; dispatch in Task 4 Step 4; hands-on is the post-branch author test. ✓

**Placeholder scan:** No TBD/TODO. The only "confirm during implementation" notes (cargo-packager exact field names in Task 3; artifact glob path in Task 4) are inherent tool-contract checks flagged in the spec, each with a concrete fallback action — not deferred work.

**Type consistency:** `export_icons(dir: &Path) -> io::Result<()>` and `icon_rgba(px: u32) -> Vec<u8>` are used consistently across Tasks 2–3. Artifact names (`*_x64-setup.exe`, `*_aarch64.dmg`, `*_x64.dmg`) match between Task 4 and Task 5. Identifier `dev.ferrolite.app` and product `FerroLite` consistent across Tasks 3–5.

**Known risk:** `image` 0.25 `IcoEncoder` writes a single-image ICO (256px master); Windows/NSIS accept and downscale it, so a multi-entry ICO is not required. If a crisper small-size taskbar icon is later wanted, generate the `.ico` from the iconset PNGs with a dedicated tool — out of scope here.
```
