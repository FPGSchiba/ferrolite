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
