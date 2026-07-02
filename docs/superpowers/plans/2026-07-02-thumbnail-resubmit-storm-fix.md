# Thumbnail Re-Submit Storm Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the thumbnail lazy-load re-submit storm by adding the missing "decoded, awaiting GPU upload" lifecycle stage (`thumb_uploading`), so `request_thumbnail` stops re-submitting/re-pushing cells whose pixels are already queued in `pending_uploads`.

**Architecture:** Add `AppState.thumb_uploading: HashSet<i64>` that mirrors "ids currently in `pending_uploads`." `request_thumbnail` dedups against it; ids enter it at every `pending_uploads` push (FastPath in `state.rs`, stash-overflow in `app.rs`) and leave it at every upload (`upload_thumbnail`); it is cleared with `pending_uploads` on reset/shutdown. The existing `FERROLITE_DIAG` module is extended with a `DedupUploading` request class and a `thumb_uploading` gauge so the fix is verifiable in the instrumented build.

**Tech Stack:** Rust, egui/eframe 0.29.1, `std::collections::HashSet`. No new dependencies.

## Global Constraints

- Behavior of `request_thumbnail`'s hot path is unchanged except for the added dedup guard: the pixel-cache fast path still pushes to `pending_uploads` + calls `ctx.request_repaint()`; the job submit + `thumb_handles.insert` are unchanged; `retain_visible_thumbnail_jobs` is NOT touched.
- The invariant to maintain: **`thumb_uploading` == the set of ids currently in `pending_uploads`.** Insert at every push to `pending_uploads`; remove at every upload (`upload_thumbnail`); clear both together in `cancel_pending_jobs`.
- `classify_request` precedence stays textured > pending > missing, with **uploading last** (before `NewSubmit`), matching `request_thumbnail`'s guard short-circuit order.
- Diagnostics stay zero-overhead when `FERROLITE_DIAG` is off (recorders already gate on `enabled()`; the new gauge is read only inside the existing `diag_t0` block).
- No `ferrolite-jobs` or `ferrolite-vt` changes. No new crate dependencies. No change to `MAX_THUMB_UPLOADS_PER_FRAME` or the round-4 cancellation.
- Rust: `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; 100-col; no `unwrap()`/`expect()` in non-test code.
- **Gate green then HOLD for the author's hands-on instrumented re-test** before finishing (CLAUDE.md).
- Windows: if `cargo test` hits `LNK1104: cannot open ...ferrolite_app-<hash>.exe`, re-run with `CARGO_TARGET_DIR=target-diag`.

---

## File Structure

| File | Responsibility | Task |
|------|----------------|------|
| `ferrolite-app/src/diag.rs` | Add `DedupUploading` outcome, 4-arg `classify_request`, `req_dedup_uploading` counter, `thumb_uploading` gauge, log/overlay display | 1 |
| `ferrolite-app/src/state.rs` | Add `thumb_uploading` field (both ctors); wire `request_thumbnail` classify call (Task 1, inert); then the guard + FastPath insert + `upload_thumbnail` remove + `cancel_pending_jobs` clear (Task 2) | 1, 2 |
| `ferrolite-app/src/app.rs` | `Gauges` construction reads `thumb_uploading.len()` (Task 1); stash-overflow branch inserts `thumb_uploading` (Task 2) | 1, 2 |

---

## Task 1: Diagnostics prep — inert `thumb_uploading` field + `DedupUploading` wiring

Adds the field (initialised empty, never populated yet) and the full diag plumbing to observe it. **Behavior is unchanged** — `thumb_uploading` stays empty because nothing inserts into it until Task 2, so `uploading` is always `false` and the guard/lifecycle are not yet active.

**Files:**
- Modify: `ferrolite-app/src/state.rs` (add field to struct + both constructors; update the `classify_request` call in `request_thumbnail`)
- Modify: `ferrolite-app/src/diag.rs` (enum, classify signature, counter, `AppCounters`, `Snapshot`, `build_snapshot`, `format_log`, `format_overlay`)
- Modify: `ferrolite-app/src/app.rs` (`Gauges` construction)

**Interfaces:**
- Produces:
  - `AppState.thumb_uploading: std::collections::HashSet<i64>`
  - `diag::ReqOutcome::DedupUploading`
  - `diag::classify_request(textured: bool, pending: bool, missing: bool, uploading: bool) -> ReqOutcome`
  - `diag::Gauges.thumb_uploading: usize`

- [ ] **Step 1: Write the failing diag test**

In `ferrolite-app/src/diag.rs`, in `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn classify_request_ranks_uploading_after_missing_before_new() {
        // uploading-only hit → DedupUploading
        assert_eq!(
            classify_request(false, false, false, true),
            ReqOutcome::DedupUploading
        );
        // none set → NewSubmit
        assert_eq!(
            classify_request(false, false, false, false),
            ReqOutcome::NewSubmit
        );
        // precedence: textured wins over uploading
        assert_eq!(
            classify_request(true, false, false, true),
            ReqOutcome::DedupTextured
        );
        // missing wins over uploading
        assert_eq!(
            classify_request(false, false, true, true),
            ReqOutcome::DedupMissing
        );
    }
```

Also update the existing `classify_request_prioritises_textured_then_pending_then_missing` test's calls to pass the new 4th argument `false` (append `, false` to each existing `classify_request(...)` call in that test).

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p ferrolite-app diag::tests::classify_request_ranks_uploading_after_missing_before_new`
Expected: FAIL — `DedupUploading` variant and the 4-arg signature don't exist.

- [ ] **Step 3: Extend the `ReqOutcome` enum and `classify_request`**

In `ferrolite-app/src/diag.rs`, add the variant:

```rust
pub enum ReqOutcome {
    NewSubmit,
    FastPath,
    DedupTextured,
    DedupPending,
    DedupMissing,
    DedupUploading,
}
```

Change `classify_request` to take `uploading` (ranked last, before `NewSubmit`):

```rust
/// Classify the outcome from the dedup guards, in `request_thumbnail`'s own
/// precedence order (textured > pending > missing > uploading). `NewSubmit` is
/// used when none of the guards hit and there is no pixel-cache fast path — the
/// caller records `FastPath` explicitly for the pixel-cache branch.
pub fn classify_request(textured: bool, pending: bool, missing: bool, uploading: bool) -> ReqOutcome {
    if textured {
        ReqOutcome::DedupTextured
    } else if pending {
        ReqOutcome::DedupPending
    } else if missing {
        ReqOutcome::DedupMissing
    } else if uploading {
        ReqOutcome::DedupUploading
    } else {
        ReqOutcome::NewSubmit
    }
}
```

- [ ] **Step 4: Add the counter, `AppCounters` field, and `record_request` arm**

In `ferrolite-app/src/diag.rs`, add the static counter beside the other `REQ_*` statics (find the block declaring `static REQ_DEDUP_MISSING: AtomicU64 = AtomicU64::new(0);` and add after it):

```rust
static REQ_DEDUP_UPLOADING: AtomicU64 = AtomicU64::new(0);
```

Add the `record_request` arm:

```rust
    let c = match outcome {
        ReqOutcome::NewSubmit => &REQ_NEW,
        ReqOutcome::FastPath => &REQ_FAST,
        ReqOutcome::DedupTextured => &REQ_DEDUP_TEX,
        ReqOutcome::DedupPending => &REQ_DEDUP_PENDING,
        ReqOutcome::DedupMissing => &REQ_DEDUP_MISSING,
        ReqOutcome::DedupUploading => &REQ_DEDUP_UPLOADING,
    };
```

Add the field to `AppCounters` (after `req_dedup_missing: u64,`):

```rust
    pub req_dedup_uploading: u64,
```

Add its load in `app_counters()` (after the `req_dedup_missing: l(&REQ_DEDUP_MISSING),` line):

```rust
        req_dedup_uploading: l(&REQ_DEDUP_UPLOADING),
```

- [ ] **Step 5: Add the `thumb_uploading` gauge, `Snapshot` field, and `build_snapshot` delta**

In `ferrolite-app/src/diag.rs`, add to `Gauges` (after `pub thumb_handles: usize,`):

```rust
    pub thumb_uploading: usize,
```

Add to `Snapshot` (after `pub req_dedup_missing_f: u64,`):

```rust
    pub req_dedup_uploading_f: u64,
```

Add to `build_snapshot`'s returned struct (after the `req_dedup_missing_f: d(...)` line):

```rust
        req_dedup_uploading_f: d(cur.req_dedup_uploading, prev_frame.req_dedup_uploading),
```

- [ ] **Step 6: Surface the new fields in `format_log` and `format_overlay`**

In `format_log`, include uploading in the dedup total and breakdown, and show the gauge. Change the `dedup` local:

```rust
    let dedup =
        s.req_dedup_tex_f + s.req_dedup_pending_f + s.req_dedup_missing_f + s.req_dedup_uploading_f;
```

Change the two affected format lines (the `thumb req/f` line and the `pending ... handles` line):

```rust
         \x20thumb req/f {req} = new {rn} + fast {rf} + dedup {dd} (tex {rt}/pend {rpd}/miss {rms}/upl {rup})\n\
         \x20      pending {tp}  uploading {tu}  handles {th}  missing {tm}  retain req {rc}\n\
```

Add the two new named args to `format_log`'s `format!` (beside `rms = ...` and `tp = ...`):

```rust
        rup = s.req_dedup_uploading_f,
        tu = g.thumb_uploading,
```

In `format_overlay`, add the uploading gauge to the thumb line and uploading to the dedup sum. Change:

```rust
         thumb pending {tp} uploading {tu} handles {th} missing {tm}\n\
```

and the `dd` arg:

```rust
        dd = s.req_dedup_tex_f + s.req_dedup_pending_f + s.req_dedup_missing_f + s.req_dedup_uploading_f,
```

and add the `tu` arg (beside `tp = g.thumb_pending,`):

```rust
        tu = g.thumb_uploading,
```

- [ ] **Step 7: Add the `thumb_uploading` field to `AppState` (both constructors)**

In `ferrolite-app/src/state.rs`, add the field to the `AppState` struct (after the `pub thumb_pending: HashSet<i64>,` / near the other thumb sets — place it right after `pub thumb_missing: HashSet<i64>,`):

```rust
    /// Image ids whose decoded pixels are queued in `pending_uploads`, awaiting
    /// GPU upload — the lifecycle bridge between a finished job (`thumb_pending`
    /// cleared on `ThumbReady`) and a live texture (`textures`). Dedups
    /// `request_thumbnail` so a cell whose pixels are already queued does not
    /// re-submit a job or re-push another copy every frame. Invariant: this set
    /// equals the ids currently in `pending_uploads`.
    pub thumb_uploading: HashSet<i64>,
```

Initialise it in **both** `AppState::new()` and `AppState::for_test()` (add beside `thumb_pending: HashSet::new(),`):

```rust
            thumb_uploading: HashSet::new(),
```

- [ ] **Step 8: Update the `classify_request` call in `request_thumbnail` (inert)**

In `ferrolite-app/src/state.rs`, `request_thumbnail`, compute `uploading` and pass it — but DO NOT add it to the early-return guard yet (Task 2 does that). Change the opening:

```rust
    pub fn request_thumbnail(&mut self, ctx: &egui::Context, image_id: i64) {
        let textured = self.textures.contains(image_id);
        let pending = self.thumb_pending.contains(&image_id);
        let missing = self.thumb_missing.contains(&image_id);
        let uploading = self.thumb_uploading.contains(&image_id);
        if textured || pending || missing {
            crate::diag::record_request(crate::diag::classify_request(
                textured, pending, missing, uploading,
            ));
            return;
        }
```

Leave the rest of `request_thumbnail` unchanged. (`uploading` is always `false` here until Task 2 populates the set, so behavior is identical; the 4-arg call keeps `diag` and `state` compiling together.)

- [ ] **Step 9: Wire the `thumb_uploading` gauge in `app.rs`**

In `ferrolite-app/src/app.rs`, find the `let gauges = crate::diag::Gauges { ... };` construction in `update` and add the field (after `thumb_handles: self.state.thumb_handles.len(),`):

```rust
                thumb_uploading: self.state.thumb_uploading.len(),
```

- [ ] **Step 10: Run tests + gate**

Run: `cargo test -p ferrolite-app`
Expected: the new `classify_request_ranks_uploading_after_missing_before_new` passes; all pre-existing tests pass (behavior unchanged — `thumb_uploading` is always empty this task).

Run: `cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean. (If clippy flags `req_dedup_uploading`/`thumb_uploading` as never-read because Task 2 hasn't activated them — it should not, since they are read by `app_counters`/`format_*`/the gauge construction — no `#[allow]` should be needed. If it does, that indicates a missed wiring above; fix the wiring rather than allow it.)

- [ ] **Step 11: Commit**

```bash
git add ferrolite-app/src/diag.rs ferrolite-app/src/state.rs ferrolite-app/src/app.rs
git commit -m "diag(app): thumb_uploading gauge + DedupUploading class (inert prep)"
```

---

## Task 2: Activate the guard + lifecycle (the fix)

Populate and consume `thumb_uploading` so the storm is actually killed.

**Files:**
- Modify: `ferrolite-app/src/state.rs` (`request_thumbnail` guard + FastPath insert; `upload_thumbnail` remove; `cancel_pending_jobs` clear both)
- Modify: `ferrolite-app/src/app.rs` (stash-overflow branch insert)
- Test: unit tests in `ferrolite-app/src/state.rs`

**Interfaces:**
- Consumes: `AppState.thumb_uploading` and `diag::classify_request` (Task 1).

- [ ] **Step 1: Write the failing behavior tests**

In `ferrolite-app/src/state.rs`, in `#[cfg(test)] mod tests`, add:

```rust
    /// A cell whose pixels are already queued for upload (`thumb_uploading`)
    /// must NOT submit a new job or push another copy to `pending_uploads`.
    #[test]
    fn request_thumbnail_dedups_ids_awaiting_upload() {
        let mut s = AppState::for_test();
        let ctx = egui::Context::default();
        s.thumb_uploading.insert(42);
        let jobs_before = s.jobs.pending_count();
        let uploads_before = s.pending_uploads.len();

        s.request_thumbnail(&ctx, 42);

        assert_eq!(
            s.jobs.pending_count(),
            jobs_before,
            "no job submitted for an id already awaiting upload"
        );
        assert_eq!(
            s.pending_uploads.len(),
            uploads_before,
            "no extra pending_uploads push for an id already awaiting upload"
        );
        assert!(
            !s.thumb_pending.contains(&42),
            "awaiting-upload id must not enter thumb_pending"
        );
    }

    /// FastPath (pixels cached, texture absent) queues the upload once and marks
    /// the id `thumb_uploading`; a repeated request is then a dedup no-op.
    #[test]
    fn request_thumbnail_fastpath_marks_uploading_once() {
        let mut s = AppState::for_test();
        let ctx = egui::Context::default();
        // Seed the CPU pixel cache (1x1 RGBA) without a live texture.
        s.thumb_pixels.insert(7, vec![1, 2, 3, 255], 1, 1);

        s.request_thumbnail(&ctx, 7);
        assert_eq!(s.pending_uploads.len(), 1, "fast path queued one upload");
        assert!(s.thumb_uploading.contains(&7), "id marked awaiting upload");

        // Second request while still awaiting upload: dedup no-op.
        s.request_thumbnail(&ctx, 7);
        assert_eq!(
            s.pending_uploads.len(),
            1,
            "no duplicate push while awaiting upload"
        );
    }

    /// Uploading an id clears its `thumb_uploading` marker (it is now textured).
    #[test]
    fn upload_thumbnail_clears_uploading_marker() {
        let mut s = AppState::for_test();
        let ctx = egui::Context::default();
        s.thumb_uploading.insert(9);

        s.upload_thumbnail(&ctx, 9, vec![1, 2, 3, 255], 1, 1);

        assert!(
            !s.thumb_uploading.contains(&9),
            "upload must clear the awaiting-upload marker"
        );
        assert!(s.textures.contains(9), "id is now textured");
    }

    /// `cancel_pending_jobs` clears both the upload queue and its guard set.
    #[test]
    fn cancel_pending_jobs_clears_uploads_and_uploading() {
        let mut s = AppState::for_test();
        s.pending_uploads.push((1, vec![0; 4], 1, 1));
        s.thumb_uploading.insert(1);

        s.cancel_pending_jobs();

        assert!(s.pending_uploads.is_empty(), "pending_uploads cleared");
        assert!(s.thumb_uploading.is_empty(), "thumb_uploading cleared");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p ferrolite-app state::tests::request_thumbnail_dedups_ids_awaiting_upload state::tests::request_thumbnail_fastpath_marks_uploading_once state::tests::upload_thumbnail_clears_uploading_marker state::tests::cancel_pending_jobs_clears_uploads_and_uploading`
Expected: FAIL — `request_thumbnail` does not yet guard on `uploading`, FastPath does not insert, `upload_thumbnail` does not remove, `cancel_pending_jobs` does not clear.

- [ ] **Step 3: Add `uploading` to the `request_thumbnail` guard + FastPath insert**

In `ferrolite-app/src/state.rs`, `request_thumbnail`, add `|| uploading` to the early-return guard and insert into `thumb_uploading` on the FastPath push:

```rust
        if textured || pending || missing || uploading {
            crate::diag::record_request(crate::diag::classify_request(
                textured, pending, missing, uploading,
            ));
            return;
        }
        // Fast path: pixels already decoded this session → re-upload directly,
        // no job / DB read / JPEG decode (Bug B). Routed through the same
        // per-frame upload budget as ThumbReady via `pending_uploads`. Mark the
        // id `thumb_uploading` so a repeat request while it waits in the queue
        // does not push another copy (re-submit storm guard).
        if let Some((rgba, w, h)) = self.thumb_pixels.get(image_id) {
            crate::diag::record_request(crate::diag::ReqOutcome::FastPath);
            self.thumb_uploading.insert(image_id);
            self.pending_uploads.push((image_id, rgba, w, h));
            ctx.request_repaint();
            return;
        }
```

(Leave the `NewSubmit` path below unchanged.)

- [ ] **Step 4: Remove from `thumb_uploading` in `upload_thumbnail`**

In `ferrolite-app/src/state.rs`, `upload_thumbnail`, remove the id from the guard set at the top of the body (after the malformed-buffer guard, before the pixel-cache insert):

```rust
        if rgba.len() != (w as usize) * (h as usize) * 4 {
            return;
        }
        // The pixels are being uploaded now → the id leaves the awaiting-upload
        // stage and (below) enters `textures`. Harmless no-op for ids uploaded
        // inline that were never queued.
        self.thumb_uploading.remove(&image_id);
        self.thumb_pixels.insert(image_id, rgba.clone(), w, h);
```

- [ ] **Step 5: Clear both in `cancel_pending_jobs`**

In `ferrolite-app/src/state.rs`, `cancel_pending_jobs`, clear the upload queue and its guard alongside `thumb_pending` (replace the final `self.thumb_pending.clear();`):

```rust
        self.thumb_pending.clear();
        // Drop any decoded-but-not-yet-uploaded thumbnails and their guard so a
        // folder switch / shutdown leaves no stale upload queue (and keeps the
        // `thumb_uploading == pending_uploads ids` invariant).
        self.pending_uploads.clear();
        self.thumb_uploading.clear();
```

- [ ] **Step 6: Insert into `thumb_uploading` at the stash-overflow push in `app.rs`**

In `ferrolite-app/src/app.rs`, `update`, the ThumbReady over-budget branch stashes to `pending_uploads`; mark the id awaiting upload there too. Change:

```rust
            if let Some((id, rgba, w, h)) = self.state.apply(event) {
                if uploads_this_frame < MAX_THUMB_UPLOADS_PER_FRAME {
                    self.state.upload_thumbnail(ctx, id, rgba, w, h);
                    uploads_this_frame += 1;
                } else {
                    // Over budget this frame — stash for a subsequent frame and
                    // mark the id awaiting upload so a re-request while it waits
                    // does not re-submit/re-push (re-submit storm guard).
                    self.state.thumb_uploading.insert(id);
                    self.state.pending_uploads.push((id, rgba, w, h));
                }
            }
```

- [ ] **Step 7: Run the behavior tests + full suite**

Run: `cargo test -p ferrolite-app`
Expected: the four new behavior tests pass; all pre-existing tests pass (including the round-4 `retain_visible_thumbnail_jobs`/`cancel_pending_jobs` tests, which are unaffected — `cancel_pending_jobs` now additionally clears the two upload fields, which those tests do not assert against).

- [ ] **Step 8: Gate**

Run: `cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-app/src/state.rs ferrolite-app/src/app.rs
git commit -m "fix(app): guard pending_uploads to kill thumbnail re-submit storm"
```

---

## Task 3: Workspace gate + author hand-off

**Files:** none (verification only).

- [ ] **Step 1: Full workspace gate**

Run:
```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all clean/green. (Windows `LNK1104` → re-run `cargo test --workspace` with `CARGO_TARGET_DIR=target-diag`.)

- [ ] **Step 2: HOLD for the author's hands-on instrumented re-test (CLAUDE.md)**

Do NOT merge/finish. Hand the author the repro and wait for feedback:

```powershell
$env:FERROLITE_DIAG = "1"; cargo run --release -p ferrolite-app
# scroll Library grid fully down, then fully back up; then close.
```

Success criteria (vs the pre-fix trace):
- `sub V` stays O(images) — a few thousand across the whole session, NOT tens of thousands.
- `backlog` (pending_uploads) stays bounded and drains — never climbs monotonically to 30k+.
- The new `uploading` gauge stays small/bounded; `req/f` shows `dedup ... /upl N` instead of `new 64` every frame.
- `pix m/s` stays low; `tex h/s` healthy; now-visible cells load promptly after the scroll-up.
- Close is prompt; `[diag close]` shows small `active`/`pending` and `joined=true`.

Address anything the author finds, then use superpowers:finishing-a-development-branch.

---

## Self-Review

**1. Spec coverage:**
- New `thumb_uploading` field + both ctors → Task 1 Step 7. ✓
- `request_thumbnail` guard (`|| uploading`) → Task 2 Step 3. ✓
- FastPath insert (push once) → Task 2 Step 3. ✓
- `upload_thumbnail` remove (single choke point) → Task 2 Step 4. ✓
- `cancel_pending_jobs` clears `pending_uploads` + `thumb_uploading` → Task 2 Step 5. ✓
- `app.rs` stash-overflow insert → Task 2 Step 6. ✓
- `events.rs` unchanged → confirmed (no task touches it). ✓
- Diagnostics: `DedupUploading` + `thumb_uploading` gauge + log/overlay → Task 1 Steps 3–6, 9. ✓
- Invariant (`thumb_uploading` == ids in `pending_uploads`) → maintained by Task 2 Steps 3/4/5/6. ✓
- Tests (guard blocks submit+push; FastPath once; upload clears; cancel clears; classify DedupUploading) → Task 2 Step 1 + Task 1 Step 1. ✓
- Manual verification criteria → Task 3 Step 2. ✓
- Non-goals (round-4 cancel, ingest gen, upload cap, jobs/vt) → untouched by all tasks. ✓

**2. Placeholder scan:** No TBD/TODO; every code step shows complete code and exact commands.

**3. Type consistency:** `thumb_uploading: HashSet<i64>` used consistently; `classify_request(bool,bool,bool,bool)` 4-arg signature matches its Task 1 call and Task 2 guard; `ReqOutcome::DedupUploading`, `AppCounters.req_dedup_uploading`, `Snapshot.req_dedup_uploading_f`, `Gauges.thumb_uploading` names match across diag definitions, `build_snapshot`, `format_log`, `format_overlay`, and the `app.rs` gauge construction. `upload_thumbnail`/`cancel_pending_jobs`/`pending_uploads` names match the current code.

> Note: Task 1 is deliberately behavior-neutral (the field is present but never populated, so `uploading` is always `false` and the guard is inactive). This gives a clean review seam — Task 1 reviews the diag plumbing, Task 2 reviews the guard/lifecycle activation. A reviewer can meaningfully approve either independently.
