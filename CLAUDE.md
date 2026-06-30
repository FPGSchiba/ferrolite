# ferrolite — repo conventions for Claude

## Responsiveness & threading (load-bearing)

1. **Never block the UI/update thread.** RAW/image decode, file & DB I/O, ingest
   directory walks, thumbnail generation, and any multi-millisecond CPU work MUST
   be submitted to `ferrolite-jobs` (with a priority + cancellation token) and
   delivered back over the app event channel, after which the job calls
   `ctx.request_repaint()`. UI-thread list/grid/filmstrip rendering MUST be
   virtualized (realize + decode only the items currently on screen) so it never
   does O(all-items) work per frame.

2. **GPU work stays on the render thread but must be bounded.** Build
   pipelines/shaders ONCE and reuse them (never rebuild per image/open/interaction);
   pre-warm expensive pipelines at startup; stream/upload incrementally (the sparse
   virtual texture) rather than in one synchronous build. Profile anything that
   could exceed a frame budget on open or navigation.

These two rules exist because both were violated and caused multi-second UI
freezes on image open — eager per-frame thumbnail decode in the Develop filmstrip
(fixed by virtualizing it), and a render-pipeline rebuild on every open (fixed by
caching pipelines in `ferrolite_vt::DisplayPipelines` and pre-warming at startup).
Keep them honored.

## Finishing a branch — wait for the author's visual test (load-bearing)

Automated checks (`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`) being green is **necessary but not sufficient** to
finish a development branch. Much of this app is egui UI whose correctness can only be
confirmed by running the real app and looking at it. Therefore: after the workspace gate
is green, **STOP and wait for the author (Jann) to visually test the running app and give
explicit feedback** before merging, pushing/PR-ing, or otherwise finishing the branch.
Do not present finish options as the final step — present them, then hold for the
author's hands-on test results, and address any issues found before completing.
