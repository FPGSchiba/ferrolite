# ferrolite — Edited-thumbnail regeneration (design)

> **Status:** Design — approved in brainstorming (2026-07-02); pending user review of this spec, then writing-plans.
> **Date:** 2026-07-02
> **Branch:** `feat/edited-thumbnails` (off `main`).
> **Problem:** The Library grid thumbnail is generated from the file's embedded preview and ignores Develop edits, so an edited image shows its *unedited* thumbnail. We want the grid to show the *edited* result.

---

## 1. Goal & validation

> Edit an image in Develop → leave the image → its Library grid thumbnail regenerates in the background from the persisted edit stack and updates in place, showing the color-managed, edited result. A right-click **Regenerate thumbnail** action force-refreshes any image(s) on demand (for the back-catalog edited before this feature shipped).

Quality is secondary to correctness/responsiveness (architecture map §2): the thumbnail is rendered from the **preview-resolution** source through the real edit pipeline — accurate to what Develop's preview shows, not a full-res re-decode.

---

## 2. Scope

**In:**
- Auto-regenerate an image's thumbnail when leaving Develop **if its edit stack changed during that session** (viewer close via Esc, navigation to another image, or switching back to Library).
- A headless **Background** regeneration job that renders the edit pipeline at thumbnail resolution and stores the result (persisted in the `thumbnails` table + refreshed in the texture cache).
- An **on-demand** "Regenerate thumbnail" grid context-menu action (single image + multi-selection), reusing the same job.

**Out (non-goals):**
- Automatic background sweep of all edited images (the on-demand action covers catch-up).
- A "thumbnail reflects edits" freshness marker / schema change.
- Full-resolution re-decode for the thumbnail (preview-res is sufficient and cheap).
- Batch **export** applying edits — this is a *separate* known gap (batch export currently uses `OpStack::default()`); tracked independently, not part of this spec.

---

## 3. Architecture of the slice

```
Develop leave (edits changed this session)  ──┐
Grid context-menu "Regenerate thumbnail"    ──┤→ spawn Background regen job (per image)
                                               │
   regen job (ferrolite-jobs @ Background, shared GpuContext — the Plan-5 batch pattern):
     1. OpStack: from the just-closed viewer (in memory) OR read the .xmp sidecar
        (ferrolite_catalog::read_ops → ferrolite_pipeline::deserialize; default = identity)
     2. decode_preview(path, kind) → preview_to_linear  (preview-resolution source)
     3. EditPipeline::new(ctx, &source, stack, camera_to_working) → render
        → blit_to_rgba8 readback (RGBA8, no whole-image f32 buffer)
     4. ferrolite_catalog::generate_thumbnail (downscale ≤256px + JPEG)
        → ThumbnailStore::put_thumbnail (persists in `thumbnails` table)
     5. tx.send(AppEvent::ThumbReady { image_id, rgba, w, h }) → ctx.request_repaint()
                                               │
   UI thread: existing ThumbReady handler replaces the cached texture (≤8 uploads/frame).
```

Mirrors the proven Plan-5 batch-export threading exactly (shared `Arc<GpuContext>` from the render state, all decode/GPU/encode inside the Background job). Reuses the existing thumbnail store + `generate_thumbnail` + `ThumbReady` upload path unchanged.

---

## 4. Trigger — "edits changed this session"

- The viewer already loads the persisted stack on open (`AppEvent::OpsLoaded`) and applies edits via the app's `apply_edit` path (app.rs). Add a per-viewer **`edits_dirty: bool`** on `ViewerState`, set `true` whenever an edit is applied to that image this session (and on reset-to-identity, which is also a change).
- On the transitions that "leave" the image — Esc-close, filmstrip/next-prev navigation to a different image, and Library-module switch — if the outgoing viewer's `edits_dirty` is set, spawn a regen job for that `image_id`, passing the final `op_stack` + `color_profile` (already in memory, avoids a sidecar read) + `path` + `kind`.
- Merely viewing an image (no edit) never regenerates.

## 5. Regeneration job

New worker in `ferrolite-app` (alongside the existing thumbnail worker in `ingest.rs` / a new `develop/thumb_regen.rs`):
```rust
pub fn regenerate_edited_thumbnail_blocking(
    writer: &Arc<Mutex<Catalog>>,
    gpu: &Arc<GpuContext>,
    image_id: i64,
    path: &Path,
    kind: FileKind,
    stack: OpStack,                 // from viewer, or read from sidecar for on-demand
    camera_to_working: [[f32; 3]; 3],
) -> Result<DecodedThumb, String>   // stores the blob + returns RGBA8 for the ThumbReady upload
```
- **Source:** `decode_preview(path, kind)` → `preview_to_linear` (the same preview thumbnails already use). For RAW, the embedded preview is sRGB-primaries, so `camera_to_working` uses the sRGB-fallback profile (matching the Develop preview tier); for the just-edited image the viewer's `color_profile` is passed through.
- **Render:** build `EditPipeline` (pipeline.rs:50) and read back via `blit_to_rgba8` (pipeline.rs:232) — bounded (small image, no whole-image f32 escalation).
- **Store + notify:** `generate_thumbnail` (thumbnail.rs:42) → `put_thumbnail` (thumbnail.rs:106) → emit `ThumbReady`.
- **On-demand path:** when the image isn't open, read the stack from its sidecar (`read_ops` → `deserialize`, default identity) and use the sRGB-fallback `camera_to_working`.

## 6. On-demand catch-up

Add "Regenerate thumbnail" to the grid context menu (`image_context_menu.rs`), scoped like the other actions: the current multi-selection if the right-clicked image is in it, else just that image. Each spawns the regen job (loading its sidecar stack). This refreshes images edited before the feature existed.

## 7. Reset case

If edits are reset to identity and the image is left, the job renders the identity pipeline → the (color-managed) unedited thumbnail. Self-healing; no special path.

---

## 8. Error handling

- Decode / render / store failure → log + keep the existing thumbnail (no `ThumbFailed`, no crash, never panic). One bad image never affects others.
- Sidecar missing / malformed → treat as identity stack (regenerate the unedited thumbnail).
- GPU device loss → the existing wgpu recovery applies; the regen job simply fails that image with a logged warning and can be re-triggered.
- All catalog writes reuse the cache-safe writer path (errors surfaced as warnings, images never lost).

---

## 9. Testing

- **Pure/unit:** the "should regenerate on leave?" predicate (`edits_dirty` gating) as a small pure function + test; `generate_thumbnail` is already unit-tested; reuse it. Any pure helper (e.g. choosing stack source) unit-tested.
- **Integration/visual:** the decode→render→store path is GPU + I/O (like the existing thumbnail worker) — not golden-tested; validated by `cargo build`/clippy + the author's hands-on visual test (edit an image, return to the grid, confirm the thumbnail reflects the edit; on-demand regenerate a back-catalog image).
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then hold for the author's visual test (CLAUDE.md).

---

## 10. Decomposition into implementation plan (for writing-plans)

1. **Regen worker + job** — `regenerate_edited_thumbnail_blocking` (decode → EditPipeline render → readback → generate_thumbnail → put_thumbnail) + a `spawn_regen_thumbnail` Background job emitting `ThumbReady`. Reuses Plan-5 batch GpuContext plumbing.
2. **Auto-trigger on leave** — `ViewerState::edits_dirty` flag (set in `apply_edit`), and spawn the regen job at the Develop-leave transitions (Esc / navigate / module switch), passing the in-memory stack + profile.
3. **On-demand context-menu action** — "Regenerate thumbnail" (single + multi-selection) in `image_context_menu.rs`, loading each image's sidecar stack.

Each is its own writing-plans → TDD cycle on this branch.

---

## 11. Key file touchpoints (from codebase mapping)

- Thumbnail worker/spawn: `ferrolite-app/src/ingest.rs:387-470`; `generate_thumbnail` `ferrolite-catalog/src/thumbnail.rs:42-127`; `ThumbReady` `ferrolite-app/src/events.rs:16-21` + upload `app.rs:1258-1266`.
- Edit persistence (sidecar): `ferrolite-catalog/src/xmp.rs` (`read_ops` 218, `write_ops` 332, `sidecar_path` 11); `ferrolite-app/src/develop/ops_persist.rs`; `ferrolite-pipeline/src/serialize.rs`.
- Edit render: `ferrolite-pipeline/src/pipeline.rs` (`EditPipeline::new` 50, `blit_to_rgba8` 232); readback reference `ferrolite-export/src/render.rs:34-93`.
- `has_edits`: `ferrolite-catalog/src/catalog.rs:280` + `ImageRecord` `model.rs:64`.
- GpuContext-on-Background-worker pattern: `ferrolite-app/src/export/batch.rs` (Plan 5).
