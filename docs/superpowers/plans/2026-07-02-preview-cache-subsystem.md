# Persistent Pipeline-Rendered Preview Cache (Improvement 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking. This plan is self-contained —
> read "Background & architecture" first. It realizes **Improvement 2** of
> `docs/superpowers/plans/2026-07-01-navigation-progressive-reveal-and-preview-cache.md`
> (Improvements 1 and 3 are already merged on this branch).

**Goal:** Make Develop browsing O(preview) regardless of megapixels or DAG depth by
caching a downscaled, color-managed ferrolite render of each RAW on disk, so second-
and-later visits (and fast filmstrip scrubbing) reveal instantly and color-correct
with no RAW decode.

**Architecture:** A new `ferrolite-previews` crate owns a content-addressed on-disk
store: a `PreviewKey` (file identity + op-stack + working space + color profile +
long edge + pipeline schema version) hashes to a filename; the payload is an 8-bit
sRGB JPEG of ferrolite's own render at a 2048 px long edge. The app looks up the key
on RAW open — hit → decode + `color_convert` (sRGB→working) → reveal immediately
(reusing the Improvement-1 sRGB reveal path), still kicking the sparse full for zoom;
miss → Improvement-1 decode+render, then async-readback the rendered preview (reusing
the histogram readback pattern), downscale/encode, and write to the store off-thread.
Invalidation is implicit: any key input change yields a new key (old entry lazily
LRU-evicted). All rendering, encode, and I/O run on `ferrolite-jobs`.

**Tech Stack:** Rust 2021, egui/eframe 0.29, wgpu 22, `image` crate (JPEG), rayon,
`ferrolite-jobs`. No new heavy dependencies — key hashing is a dependency-free stable
FNV-1a over canonical bytes.

## Global Constraints

- **Cache is RAW-ONLY.** Standard (JPG/PNG) images have no tier-2 and their reveal IS
  the full-resolution image; a 2048 px cache would *downgrade* them. Guard every cache
  path on `kind == ferrolite_image::FileKind::Raw`. Standard open/reveal is untouched.
- **Storage format:** 8-bit sRGB JPEG, quality 90, long edge **2048 px** (constant
  `PREVIEW_LONG_EDGE: u32 = 2048`). The stored image is ferrolite's *own* color-managed
  render encoded to sRGB — NOT the camera embedded JPEG — so decoding it back through
  `color_convert` (sRGB→working) reproduces the color-correct render (minus 8-bit
  quantization). This is why a cache hit shows no color/tone shift.
- **Threading (CLAUDE.md, load-bearing):** all cache rendering, JPEG encode/decode, and
  file I/O run on `ferrolite-jobs` with a priority + cancellation token, delivered over
  the app event channel; NEVER on the UI/update thread. GPU readback of the rendered
  preview uses the existing async pattern (`read_async` + `Maintain::Poll`), never a
  blocking map. Build GPU pipelines once and reuse (do not compile per open).
- **Cache location:** `<base>/ferrolite/previews/` where `<base>` is the same dir
  root that `default_db_path()` (in `ferrolite-app/src/state.rs`) uses for
  `catalog.db` (`$LOCALAPPDATA` → `$XDG_DATA_HOME` → `$HOME` → `.`). Pass the resolved
  cache dir in from the app; the crate itself takes a `&Path` and never resolves env.
- **Size cap:** default **2 GiB** (`DEFAULT_CACHE_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024`),
  bounded LRU eviction by last-access time. A "purge previews" action clears the dir.
- **Schema version:** `PIPELINE_SCHEMA_VERSION: u32` in `ferrolite-previews`; any change
  to the render pipeline that alters preview pixels bumps it, invalidating all entries.
- `cargo fmt --all` clean; `cargo clippy --workspace --all-targets -- -D warnings`
  exit 0; `cargo test --workspace` green after each task. Conventional commits, no
  attribution footer.
- **Finish rule (CLAUDE.md):** a green workspace gate is necessary but not sufficient —
  STOP and wait for the author's visual test before finishing the branch.

---

## Background & architecture (read first)

**Where it hooks in the app (all `ferrolite-app/src`):**
- `apply_full_decoded` (`app.rs`, ~L482) builds the RAW rung-1 reveal render (an
  `EditPipeline` over the demosaic with `cam` + op stack) and the sparse full. This is
  the **cache write-back site** (on a miss): after the rung-1 render exists, async-read
  it back, encode, and write.
- The RAW open path (`viewer/load.rs` `spawn_full`, dispatched from `app.rs`'s open/
  navigation handling, debounced by `FULL_DECODE_DEBOUNCE`) is where the **cache read**
  goes: compute the key, on hit reveal from the cached JPEG and skip the RAW decode
  (still kick the sparse full for zoom); on miss fall through to today's decode.
- `viewer/nav.rs` `neighbor_in_set(ids, current, dir)` already yields ordered neighbors
  — the **prefetch** source.
- Invalidation needs no explicit delete: `persist_ops` (edit commit) and
  `apply_working_space` change the key inputs, so the next visit is a natural miss that
  re-renders and re-caches; the stale entry is LRU-evicted later.
- Async GPU readback precedent: `maybe_update_histogram` (`app.rs`, ~L152) dispatches a
  GPU pass and calls `vp.histogram.read_async(move |bins| { tx.send(...); repaint })`,
  polling with `Maintain::Poll`. The write-back reuses this shape to pull the rendered
  preview pixels off the GPU without blocking.

**The cached payload is display-encoded sRGB of OUR render.** On the reveal render the
single VT texture is `Rgba16Float` in *working* space. To store it we apply
working→display (`working_to_display(working_space)`) + sRGB OETF and 8-bit quantize;
on read we do sRGB→working (`preview_to_working` / `color_convert`, the exact path
Improvement 1 uses for Standard/JPEG reveal) to get back to working-space linear. Round-
tripping our own render through sRGB is color-consistent; round-tripping the *camera*
JPEG was not — that distinction is the whole point.

**File layout:** `<cache>/<digest>.jpg` (payload) + `<cache>/<digest>.key` (the full
`PreviewKey` as JSON). `digest` is a 16-hex-char FNV-1a-64 of the key's canonical bytes.
On read, digest→file, then exact-compare the `.key` to guard the (negligible) hash
collision; mismatch = miss. Writes are atomic (temp file + rename).

---

## File structure

New crate `ferrolite-previews`:
- `ferrolite-previews/Cargo.toml` — deps: `ferrolite-image`, `ferrolite-color` (for the
  display matrix + sRGB), `image` (jpeg), `serde`/`serde_json`, `rayon` (downscale). All
  workspace deps.
- `src/lib.rs` — re-exports; `PREVIEW_LONG_EDGE`, `DEFAULT_CACHE_CAP_BYTES`,
  `PIPELINE_SCHEMA_VERSION`.
- `src/key.rs` — `PreviewKey`, canonical bytes, FNV-1a digest, `WorkingSpaceId`/profile
  hashing helpers.
- `src/codec.rs` — encode working-space `LinearRgbaF32` → 8-bit sRGB JPEG bytes at a
  long edge; decode JPEG bytes → sRGB `ImageBuffer` (for the app to `color_convert`).
- `src/store.rs` — `PreviewStore` (get/put/contains) over a `&Path` dir, atomic writes,
  `.key` verification.
- `src/lru.rs` — size accounting + eviction to a cap by last-access time.

App wiring (`ferrolite-app/src`):
- `src/develop/preview_cache.rs` (new) — job spawns: `spawn_cache_write` (encode+store),
  `spawn_cache_read` (lookup+decode), `spawn_prefetch`; `AppEvent` variants; key assembly
  from `ViewerState`.
- `src/app.rs`, `src/viewer/load.rs`, `src/events.rs`, `src/state.rs` — hook points.

---

## Task 1: `ferrolite-previews` crate skeleton + `PreviewKey` + stable digest

**Files:**
- Create: `ferrolite-previews/Cargo.toml`, `ferrolite-previews/src/lib.rs`,
  `ferrolite-previews/src/key.rs`
- Modify: root `Cargo.toml` (add `"ferrolite-previews"` to `members` and a
  `ferrolite-previews = { path = "ferrolite-previews" }` workspace dep entry)

**Interfaces — Produces:**
```rust
pub const PREVIEW_LONG_EDGE: u32 = 2048;
pub const DEFAULT_CACHE_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const PIPELINE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct PreviewKey {
    pub file_size: u64,
    pub file_mtime_ns: i64,
    pub op_stack_hash: u64,
    pub working_space: u8,
    pub color_profile_hash: u64,
    pub preview_long_edge: u32,
    pub schema_version: u32,
}
impl PreviewKey {
    /// 16-hex-char FNV-1a-64 over canonical little-endian field bytes.
    pub fn digest(&self) -> String;
}
/// FNV-1a-64 over arbitrary bytes (dependency-free, stable across runs/versions).
pub fn fnv1a_64(bytes: &[u8]) -> u64;
/// Hash any serde-serializable value via canonical serde_json bytes → fnv1a_64.
pub fn hash_serde<T: serde::Serialize>(value: &T) -> u64;
```

- [ ] **Step 1 — Write failing tests** (`ferrolite-previews/src/key.rs` `#[cfg(test)]`):
  - `digest_is_stable`: a fixed `PreviewKey` produces a specific constant 16-char hex
    digest (compute once, pin it) — proves cross-run stability.
  - `digest_changes_when_any_field_changes`: for each of the 7 fields, mutating it alone
    changes the digest (loop/parametrize; assert all 7 differ from the base).
  - `hash_serde_is_order_stable`: `hash_serde` of the same value twice is equal, and of
    two structurally-different values differs.
- [ ] **Step 2 — Run tests, verify they FAIL** (`cargo test -p ferrolite-previews`;
  expect unresolved `PreviewKey`/`fnv1a_64`).
- [ ] **Step 3 — Implement** `Cargo.toml`, `lib.rs` (consts + re-exports), `key.rs`:
  `fnv1a_64` (offset basis `0xcbf29ce484222325`, prime `0x100000001b3`), `hash_serde`
  (`serde_json::to_vec` → `fnv1a_64`), `PreviewKey::digest` (append each field's
  `to_le_bytes` in declaration order → `fnv1a_64` → `format!("{:016x}")`).
- [ ] **Step 4 — Run tests, verify PASS.** Pin the `digest_is_stable` constant from the
  actual first run.
- [ ] **Step 5 — Verify gate:** `cargo fmt --all`, `cargo clippy -p ferrolite-previews
  --all-targets -- -D warnings`, `cargo test -p ferrolite-previews`.
- [ ] **Step 6 — Commit:** `feat(previews): ferrolite-previews crate + stable PreviewKey digest`.

**Acceptance:** crate builds in the workspace; digest is stable and changes on every key
input; hashing is dependency-free.

---

## Task 2: 8-bit sRGB JPEG codec (encode render → bytes; decode bytes → sRGB buffer)

**Files:**
- Create: `ferrolite-previews/src/codec.rs`; add `mod codec; pub use` to `lib.rs`.

**Interfaces — Consumes:** `ferrolite_image::{LinearRgbaF32, ImageBuffer, PixelFormat}`,
`ferrolite_color::working_to_display`. **Produces:**
```rust
/// Encode a WORKING-space linear render to an 8-bit sRGB JPEG (quality 90),
/// downscaled so its long edge == `long_edge` (never upscaled). `display_matrix`
/// is `working_to_display(working_space)`; sRGB OETF is applied after it.
pub fn encode_srgb_jpeg(
    render: &LinearRgbaF32,
    display_matrix: [[f32; 3]; 3],
    long_edge: u32,
    quality: u8,
) -> Result<Vec<u8>, PreviewCodecError>;

/// Decode JPEG bytes → 8-bit sRGB `ImageBuffer` (Rgb8) for the app to color_convert.
pub fn decode_srgb_jpeg(bytes: &[u8]) -> Result<ImageBuffer, PreviewCodecError>;
```

- [ ] **Step 1 — Failing tests** (`codec.rs` `#[cfg(test)]`):
  - `encode_downscales_long_edge`: a 4096×2048 render encodes to a JPEG whose decoded
    dims have long edge 2048 (here 2048×1024), aspect preserved.
  - `encode_never_upscales`: a 512×256 render stays 512×256.
  - `roundtrip_is_color_close`: encode a known mid-gray *working-linear* value with
    identity `display_matrix`, decode, and assert the decoded 8-bit sRGB value is within
    ±2 codes of the expected sRGB OETF of that linear value (proves matrix+OETF applied,
    not raw-linear stored).
  - `decode_rejects_garbage`: `decode_srgb_jpeg(&[0,1,2])` is `Err`.
- [ ] **Step 2 — Run, verify FAIL.**
- [ ] **Step 3 — Implement:** apply `display_matrix` (3×3) per pixel to working-linear,
  clamp [0,1], sRGB OETF → u8; compute target dims (`long_edge` scaling, `min` to avoid
  upscale); box/triangle downscale (use `image::imageops::resize` with
  `FilterType::Triangle`, or rayon row resample) to `Rgb8`; JPEG-encode via
  `image::codecs::jpeg::JpegEncoder::new_with_quality` (mirror
  `ferrolite-catalog/src/thumbnail.rs`). Decode via `image::load_from_memory` → `to_rgb8`
  → `ImageBuffer::new(.., PixelFormat::Rgb8, ..)`. Define `PreviewCodecError` (thiserror).
- [ ] **Step 4 — Run, verify PASS.**
- [ ] **Step 5 — Gate** (`-p ferrolite-previews`).
- [ ] **Step 6 — Commit:** `feat(previews): 8-bit sRGB JPEG codec for cached renders`.

**Acceptance:** round-trips a working-space render to color-consistent 8-bit sRGB at the
standard long edge; rejects invalid input.

---

## Task 3: `PreviewStore` — content-addressed on-disk store (get/put/contains, atomic, verified)

**Files:**
- Create: `ferrolite-previews/src/store.rs`; wire into `lib.rs`.

**Interfaces — Consumes:** `PreviewKey`. **Produces:**
```rust
pub struct PreviewStore { dir: PathBuf }
impl PreviewStore {
    pub fn new(dir: &Path) -> std::io::Result<Self>; // creates dir if absent
    pub fn contains(&self, key: &PreviewKey) -> bool; // digest file exists AND .key matches
    /// Returns the payload JPEG bytes on an exact-key hit; touches last-access on hit.
    pub fn get(&self, key: &PreviewKey) -> Option<Vec<u8>>;
    /// Atomically writes `<digest>.jpg` + `<digest>.key` (temp + rename).
    pub fn put(&self, key: &PreviewKey, jpeg: &[u8]) -> std::io::Result<()>;
    pub fn total_bytes(&self) -> u64;          // sum of *.jpg sizes
    pub fn purge_all(&self) -> std::io::Result<()>;
}
```

- [ ] **Step 1 — Failing tests** (tempdir):
  - `put_then_get_roundtrips`: `put(key, bytes)` then `get(key) == Some(bytes)`.
  - `get_miss_on_absent`: fresh store, `get(key) == None`, `contains == false`.
  - `key_mismatch_on_digest_collision_is_miss`: write a `.jpg` + a `.key` whose JSON is a
    *different* key at the same digest filename (simulate a collision by writing files
    manually), and assert `get(original_key) == None` (the `.key` compare rejects it).
  - `put_is_atomic_no_partial`: after `put`, no `*.tmp` remains in the dir.
  - `total_bytes_sums_payloads`: two puts → `total_bytes` == sum of the two JPEG lengths.
- [ ] **Step 2 — Run, verify FAIL.**
- [ ] **Step 3 — Implement:** `get` reads `<digest>.key`, `serde_json`-parses, compares
  `== *key`; on match reads+returns `<digest>.jpg` and `filetime`-touches it (use
  `std::fs::File::open` + set mtime via `std::fs` — implement `touch` by rewriting mtime
  with `std::fs::OpenOptions` + a portable `filetime`? AVOID new dep: instead track access
  by writing a 0-byte `<digest>.at` sidecar's mtime, OR simply re-`open`+read which does
  not update mtime portably — see Task 4 note). To stay dependency-free, record access
  time as the payload file's own mtime by re-writing it is heavy; INSTEAD keep a small
  in-dir `access.log`? **Decision:** use the `.key` file's mtime as last-access and update
  it with `std::fs::File::set_times` (stable since Rust 1.75) on every `get` hit. `put`
  writes payload+key via `NamedTempFile`-style (write `.tmp`, `rename`).
- [ ] **Step 4 — Run, verify PASS.**
- [ ] **Step 5 — Gate.**
- [ ] **Step 6 — Commit:** `feat(previews): content-addressed PreviewStore with atomic verified writes`.

**Acceptance:** round-trip; collision-safe via `.key` verify; atomic writes; byte
accounting.

---

## Task 4: LRU eviction to a size cap

**Files:**
- Create: `ferrolite-previews/src/lru.rs`; add `PreviewStore::evict_to(cap)` using it.

**Interfaces — Produces:**
```rust
/// Pure planner: given (digest, bytes, last_access_ns) entries and a cap, return the
/// digests to delete (oldest last_access first) so the remaining total <= cap.
pub fn plan_eviction(entries: &[(String, u64, i64)], cap_bytes: u64) -> Vec<String>;
// on PreviewStore:
pub fn evict_to(&self, cap_bytes: u64) -> std::io::Result<u64>; // returns bytes freed
```

- [ ] **Step 1 — Failing tests** (`lru.rs` pure `plan_eviction`):
  - `no_eviction_when_under_cap`: total < cap → empty plan.
  - `evicts_oldest_first_until_under_cap`: 3 entries, cap forces dropping the two oldest
    by `last_access`; assert exact digests + order.
  - `evicts_all_when_cap_zero`: cap 0 → all digests returned.
  - `ties_break_deterministically`: equal `last_access` → stable order (e.g. by digest).
  - Store test `evict_to_deletes_payload_and_key`: after `evict_to`, both `.jpg` and
    `.key` for evicted digests are gone and `total_bytes <= cap`.
- [ ] **Step 2 — Run, verify FAIL.**
- [ ] **Step 3 — Implement:** `plan_eviction` sorts by `(last_access, digest)` ascending,
  accumulates from newest until `<= cap`, returns the rest. `evict_to` scans the dir
  (`*.key` mtime = last-access, paired `*.jpg` size), calls `plan_eviction`, deletes both
  files per evicted digest.
- [ ] **Step 4 — Run, verify PASS.**
- [ ] **Step 5 — Gate.**
- [ ] **Step 6 — Commit:** `feat(previews): bounded LRU eviction to size cap`.

**Acceptance:** bounded cache; oldest-first deterministic eviction; frees both files.

---

## Task 5: App wiring — key assembly + cache write-back on miss (async readback)

**Files:**
- Create: `ferrolite-app/src/develop/preview_cache.rs`
- Modify: `ferrolite-app/src/app.rs` (`apply_full_decoded` write-back hook; owns a
  `PreviewStore`), `ferrolite-app/src/events.rs` (new `AppEvent` variants),
  `ferrolite-app/src/state.rs` (resolve cache dir next to `catalog.db`; construct store),
  `ferrolite-app/Cargo.toml` (add `ferrolite-previews`).

**Interfaces — Produces (in `preview_cache.rs`):**
```rust
/// Build the key for the currently-open RAW image from viewer + app state.
pub fn key_for(
    path: &std::path::Path,
    op_stack: &ferrolite_pipeline::OpStack,
    working_space: ferrolite_color::WorkingSpace,
    color_profile: &ferrolite_decode::ColorProfile,
) -> std::io::Result<ferrolite_previews::PreviewKey>; // file_size/mtime from fs::metadata
/// Spawn a low-latency job to encode `render` and store it under `key`, then evict_to cap.
pub fn spawn_cache_write(
    jobs: &Arc<JobSystem>, store: Arc<PreviewStore>, ctx: &egui::Context,
    key: PreviewKey, render: LinearRgbaF32, display_matrix: [[f32;3];3], cap: u64,
);
```
`key_for` maps `working_space` → `u8` discriminant and hashes `op_stack`
(`hash_serde`) and `color_profile` (`hash_serde` if `ColorProfile: Serialize`, else hash
its `xyz_to_cam` + `white_xy` bytes) — see the note in Task 1's helpers.

- [ ] **Step 1 — Failing test** (`preview_cache.rs` `#[cfg(test)]`, pure `key_for`):
  `key_for_is_stable_and_input_sensitive` — same inputs → equal key; different op_stack /
  working_space / profile → different key. (Use a temp file for `path` so
  `fs::metadata` works; assert `file_size`/`mtime` populated.)
- [ ] **Step 2 — Run, verify FAIL.**
- [ ] **Step 3 — Implement** `key_for` + `spawn_cache_write` (job: `encode_srgb_jpeg` →
  `store.put` → `store.evict_to(cap)`; on error `eprintln!` and drop — a cache failure
  must never break the viewer; `ctx.request_repaint()` optional). Resolve the cache dir in
  `state.rs` as `<base>/ferrolite/previews` (reuse the `default_db_path` base logic; factor
  a `default_previews_dir()` beside it) and build `Arc<PreviewStore>` at startup; store it
  on `AppState`. Add the write-back hook in `apply_full_decoded`: guard on RAW; after the
  rung-1 render texture is built, if the open was a **cache miss** (flag passed through from
  Task 6's read, default true), async-read the render back (reuse the histogram
  `read_async` + `Maintain::Poll` pattern on a copy of the rung-1 render — OR, simpler and
  fully off-GPU, encode directly from the CPU-side demosaic `image` run through
  `cam`→working→display: **preferred** — you already hold `image: LinearRgbaF32` and can
  compute the display matrix, so call `spawn_cache_write` with `image` transformed by the
  op stack? NOTE: op-stack application is GPU-only, so for the FIRST cut cache the
  **identity-stack** render (unedited color-managed preview) computed on CPU from `image` +
  `cam`·`working_to_display`; the key's `op_stack_hash` then reflects the actual stack, so
  an edited image is a deliberate miss until Task 8 adds edited-render readback). Document
  this scoping decision inline. Add `AppEvent::PreviewCacheWritten { image_id }` (for tests/
  metrics; handler may be a no-op + repaint).
- [ ] **Step 4 — Run, verify PASS** (`cargo test -p ferrolite-app preview_cache`).
- [ ] **Step 5 — Gate** (`cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`).
- [ ] **Step 6 — Commit:** `feat(app): preview-cache write-back on RAW open miss`.

**Acceptance:** opening an uncached RAW writes a color-correct 2048 sRGB JPEG under the key;
cache dir sits next to `catalog.db`; a cache failure never disturbs the viewer; workspace
green.

> **Scoping note (important):** Task 5 caches the **identity/committed-on-CPU** render.
> Caching the *GPU op-stack* render requires reading the rendered texture back (Task 8).
> This keeps Task 5 fully off the render thread and immediately useful for the dominant
> unedited-browse case. Do NOT block Task 5 on GPU readback.
>
> **CORRECTNESS GUARD (do not skip):** because Task 5 renders the *identity* preview on
> CPU but `key_for` hashes the *actual* op stack, the write-back MUST be gated on
> `v.op_stack == ferrolite_pipeline::OpStack::default()`. Storing an identity render under
> an *edited* key would make a later cache hit reveal the wrong (unedited) image. Edited
> images are therefore a deliberate cache miss (live render every visit) until Task 8 reads
> back the real GPU render; for a default stack the identity render equals the displayed
> reveal, so the entry is exactly correct. Cache **read** (Task 6) needs no guard — an
> edited key was never written, so it naturally misses. The prefetch in Task 7 keys by the
> **default** stack (it does not read each neighbor's sidecar); an edited neighbor's
> default-keyed entry is simply never requested until that neighbor is reset — harmless,
> LRU-evicted. Add a short `#[cfg(test)]` assertion in Task 5 that write-back is skipped
> when the stack is non-default.

---

## Task 6: Cache read on RAW open — reveal from cache, skip decode

**Files:**
- Modify: `ferrolite-app/src/develop/preview_cache.rs` (`spawn_cache_read`),
  `ferrolite-app/src/events.rs` (`AppEvent::PreviewCacheHit`/`Miss`),
  `ferrolite-app/src/app.rs` (open flow: consult cache before `spawn_full`; reveal on hit),
  `ferrolite-app/src/viewer/load.rs` if the open dispatch lives there.

**Interfaces — Produces:**
```rust
/// Look up `key`; on hit decode to an sRGB ImageBuffer and send
/// AppEvent::PreviewCacheHit { image_id, srgb }; else AppEvent::PreviewCacheMiss { image_id }.
pub fn spawn_cache_read(
    jobs: &Arc<JobSystem>, store: Arc<PreviewStore>, tx: &Sender<AppEvent>,
    ctx: &egui::Context, image_id: i64, key: PreviewKey,
);
```

- [ ] **Step 1 — Failing test:** `spawn_cache_read` over a store pre-seeded (via
  `PreviewStore::put`) with a known JPEG for `key` sends `PreviewCacheHit` carrying a
  decoded buffer of the expected dims; an absent key sends `PreviewCacheMiss`. (Drive the
  job synchronously in-test or via a test `JobSystem`; assert on the channel.)
- [ ] **Step 2 — Run, verify FAIL.**
- [ ] **Step 3 — Implement:** on RAW open (before/instead of the debounced `spawn_full`),
  compute `key_for` and `spawn_cache_read`. Handle events in `app.rs`:
  - `PreviewCacheHit { image_id, srgb }`: build the rung-1 preview from `srgb` via the
    **existing Improvement-1 sRGB reveal path** (`preview_to_linear`/`color_convert` with
    `preview_to_working`), set `loaded = true`, fit, insert `ViewerGpu`, mark histogram
    dirty — i.e. reuse `reveal_srgb_preview`. Then still kick the sparse full (`spawn_full`)
    for zoom detail, but with the write-back flag = **false** (already cached). Guard stale
    `image_id`.
  - `PreviewCacheMiss { image_id }`: proceed with today's `spawn_full` decode+render path,
    write-back flag = **true** (Task 5 caches the result).
  Keep `FULL_DECODE_DEBOUNCE` + `cancel_loads`/`cancel_sparse` so scrubbing coalesces and
  cache reads for scrubbed-past images cancel.
- [ ] **Step 4 — Run, verify PASS.**
- [ ] **Step 5 — Gate.**
- [ ] **Step 6 — Commit:** `feat(app): reveal RAW from preview cache on hit, skip decode`.

**Acceptance:** a second visit to a RAW reveals from cache with no RAW decode and no color
shift; a miss falls through to decode+render+cache; scrubbing still coalesces and cancels.

---

## Task 7: Neighbor prefetch + purge action

**Files:**
- Modify: `ferrolite-app/src/develop/preview_cache.rs` (`spawn_prefetch`),
  `ferrolite-app/src/app.rs` (trigger prefetch on selection settle; wire a "Purge previews"
  action), `ferrolite-app/src/viewer/nav.rs` (reuse `neighbor_in_set`).

**Interfaces — Produces:**
```rust
/// For the N nearest neighbors (both directions) not already cached, spawn LOW-priority
/// jobs that decode+render+store their identity preview. Cancellable; skips cache hits.
pub fn spawn_prefetch(
    jobs: &Arc<JobSystem>, store: Arc<PreviewStore>, ctx: &egui::Context,
    neighbors: &[(i64, std::path::PathBuf)], /* + params to build keys */ radius: usize,
);
```

- [ ] **Step 1 — Failing tests** (pure helper): a `prefetch_targets(ids, current, radius)`
  returns the correct ordered neighbor ids within radius, clamped at list ends, excluding
  `current`. (Reuse/extend `nav.rs`; assert middle, both ends, radius > remaining.)
- [ ] **Step 2 — Run, verify FAIL.**
- [ ] **Step 3 — Implement:** `prefetch_targets` (pure), and `spawn_prefetch` that, for each
  target lacking a cache entry (`store.contains`), submits a `Priority::Background`/lowest
  job that decodes (reuse `decode_full` + demosaic), renders the identity preview on CPU
  (same as Task 5), and `store.put` + `evict_to`. Trigger `spawn_prefetch` when the current
  selection settles (after reveal, once idle), radius 2. Add a "Purge previews" action
  (menu/settings) calling `store.purge_all()` off-thread. All prefetch jobs must be
  cancellable on navigation (share the load cancellation set).
- [ ] **Step 4 — Run, verify PASS.**
- [ ] **Step 5 — Gate.**
- [ ] **Step 6 — Commit:** `feat(app): low-priority neighbor prefetch + purge-previews action`.

**Acceptance:** after opening a RAW, its filmstrip neighbors get cached in the background at
low priority without stealing interactivity; scrubbing past cancels pending prefetch; purge
clears the cache dir.

---

## Task 8 (optional / follow-up): cache the GPU op-stack render via async readback

**Files:** `ferrolite-app/src/develop/preview_cache.rs`, `app.rs`.

Replace Task 5's CPU identity-render write-back with an async readback of the actual
rung-1 GPU render (op stack applied), so **edited** images also cache correctly. Reuse the
histogram `read_async` + `Maintain::Poll` pattern: after the rung-1 `EditPipeline`
evaluates, dispatch a readback of its texture (working-space `Rgba16Float`), and on the
async callback hand the pixels to `spawn_cache_write` (which applies working→display + sRGB
+ downscale + JPEG). Guard bounded-per-frame; never block. Add a test that an edited
op-stack produces a *different* cached payload than identity for the same file.

- [ ] Steps mirror Tasks 5–6 (failing test on payload difference → implement async readback
  → verify → gate → commit `feat(app): cache edited GPU render via async readback`).

**Acceptance:** editing a RAW and revisiting reveals the *edited* look from cache; identity
and edited renders cache under distinct keys.

---

## Suggested order & stopping point

Tasks 1–4 are the pure `ferrolite-previews` crate (no GPU/UI; fully unit-tested — ideal for
cheap implementer models). Tasks 5–7 are app integration (standard model). Task 8 is an
optional follow-up (async GPU readback) — ship 1–7 first, get the author's visual test
(open a folder of RAWs twice: first pass caches, second pass reveals instantly & color-
correct; scrub a large folder; purge and confirm rebuild), then decide whether edited-render
caching (Task 8) is worth it.

## Self-review checklist (done during planning)
- Every Improvement-2 spec bullet maps to a task: key+layout (T1,T3), render-to-cache
  off-thread (T5), read-on-open+prefetch (T6,T7), invalidation via key inputs +
  schema_version (T1,T5) + implicit re-render, LRU+cap+purge (T4,T7), round-trip/stability/
  eviction tests (T1–T4). ✅
- No placeholders; interfaces name exact types; RAW-only + threading + format constraints
  are stated once in Global Constraints and bind every task. ✅
- Known risk flagged: op-stack render is GPU-only, so T5 caches the identity/CPU render and
  T8 (async readback) covers edited renders — the key's op_stack_hash keeps correctness
  either way (edited = deliberate miss until T8). ✅
