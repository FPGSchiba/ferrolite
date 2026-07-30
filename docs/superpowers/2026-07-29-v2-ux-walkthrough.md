# FerroLite V2 — UX Walkthrough (workstream 2)

> **How to use this:** run the app and work through each item at your own pace, in any order.
> Every item names the steps to get there and what to *judge* — not what's broken, but how it
> feels: discoverability, friction, visual rhythm, whether your hand goes where the control is.
> Write anything under the `Verdict:` lines — freeform is fine ("fine", "annoying because…",
> "move this", screenshots welcome). Skip items that don't apply. Your annotations become the
> `systematic-ui-fixes-round-4` spec; accepted changes also flow back into `docs/design/V2/README.md`.
>
> Fixtures worth having in the catalog before you start: one hazy landscape RAW, one high-ISO
> shot, one blown-sky sunset (bright saturated color), one JPG, and a folder with 200+ images
> for scrolling feel.

---

## 1. Library

**1.1 First-glance density & rhythm.** Open Library on the 200+ folder. Judge: grid density at
the default Size, whitespace balance, whether filename/date rows under thumbnails read as one
unit or clutter. Try the Size slider through its whole range.
Verdict: Feels and looks good

**1.2 Culling flow.** Rate (1-5), flag pick/reject, and color-label a dozen images using only the
keyboard, then only the mouse. Judge: does the flow interrupt you anywhere; are the overlays on
thumbnails (stars, dot) readable on both light and dark images?
Verdict: Feels and looks good

**1.3 Left panel trees.** Fold/unfold Folders and Collections; drag a collection into a set;
drag photos into a collection. Judge: hit-target sizes, indent readability, drop-target clarity,
count badges.
Verdict: Trees are fine. I would like a drag and drop to establish or abolish parent child collection relationships.

**1.4 Filters & search.** Use the toolbar rating/flag filters, the Metadata popup (camera/lens/
ISO ranges), and search. Judge: can you compose "picks with ≥3 stars from this lens" without
thinking; does the Metadata popup earn its separate home vs. living in the toolbar?
Verdict: Metedatafilter directly closes when I press on any dropdown. There is also a useless icon to the right of the metadata filter. Additionally I would like a reset button for all filters. Does subfolders belong where it currently is? I think it feels a bit wrong. The ranges need to be improved with a slider where ranges can be defined. I will never use it otherwise. The rest looks good to me. File type filter needs to be resetable in the UI as well. And multiple file types should also be filterable.

**1.5 Status & feedback.** During an ingest of a new folder: judge the progress feedback, and
whether the app ever feels "busy without saying why."
Verdict: I am happy currently. We can come back to this once we start work on V3.

## 2. Develop — Adjust scope

**2.1 Panel architecture.** Light / Color / Effects tabs: does the 3-tab consolidation hold up in
daily use, or do you miss a dedicated place for anything? Judge section ORDER within each tab
(Basic → Tone Curve; HSL → Color Mix → Grading; Sharpen → NR → Dehaze → Optics).
Verdict: What I don't understand is why we have some sliders duplicated underneath the tone curve. Either make that more clear or remove one set of sliders.

**2.2 H/S/W/B coexistence (flagged for your explicit verdict).** Light tab: BASIC SLIDERS'
Highlights/Shadows/Whites/Blacks (region-based, new in Phase 3) vs. the parametric H/S/W/B
sliders inside TONE CURVE. Two different algorithms, Lightroom-style precedent. Judge: is the
coexistence discoverable/coherent, or does one of them need renaming/moving/removing?
Verdict: No I think its not clear. Maybe it needs its own section like the tone curve. That would give us the option to add a small sub header to explain the difference. Thats my point from above.

**2.3 Slider feel.** Drag-anywhere, double-click reset, the per-control reset arrows, value
readouts. Judge on Exposure (bipolar EV), Dehaze (bipolar), Sharpen radius (stepped px).
Verdict: Somehow there are no longer any edits on JPG Files and there are no longer any live edits. So when I drag the slider only after I release the effect is applied. Tone curves are correct, but all sliders on every tab does not work live.

**2.4 Tone curve & wheels.** Point-curve editing (add/drag/delete points, channel switch), the
parametric section, and the four grading wheels + Blending/Balance. Judge: precision vs.
fiddliness at panel width, wheel hit-targets, Lum slider placement under each wheel.
Verdict: Feels good.

**2.5 Greyed-with-reason convention.** Hover the NR sliders (greyed everywhere). Judge: does the
hover reason read as a promise or an apology; is greyed-but-visible better than hidden for you?
Verdict: No greyed out is fine for me.

**2.6 Working-space / chrome row.** The camera name + save-state + working-space combo above the
tabs. Judge: is "Saved/No edits" trustworthy-feeling; is Working space discoverable enough for
how rarely it's touched?
Verdict: I do not like the big space to the right. The scrollbar is moved to the right about 15px or more. It looks a bit off. Also panning and zooming flashes the edited image in and out with the raw.

## 3. Develop — Mask scope (the unified options library)

**3.1 The core vision.** Mask tool → create/paint a mask → edit Exposure, then switch to Adjust
and back. Judge: does "same tabs, scoped by the banner" stay unmistakable after 10 minutes of
mixed editing, or have you ever edited the wrong scope by accident? Is the banner's accent
enough, or does the panel need a stronger scope signal (tint? border?)?
Verdict: Feels good.

**3.2 Mask management block.** Create, rename (double-click), invert, visibility-eye, delete;
the components row + Components modal; New Brush Layer. Judge: is the block compact enough above
the tabs; is the Components modal the right home for component-level work?
Verdict: Feels and looks good.

**3.3 Brush & overlay feel.** Paint with Ctrl+scroll size changes; toggle the red overlay (M);
drag a slider mid-overlay (overlay hides while dragging). Judge: brush cursor feedback,
overlay color/opacity, the hide-while-adjusting behavior.
Verdict: Feels and looks good.

**3.4 Range masks.** Luminance and Color range components: pick ranges/samples, judge the
lo/hi/softness controls, and how selections track when you change the global tone curve.
Verdict: Feels and looks good.

**3.5 Per-mask neighborhood ops.** Per-mask Sharpen (try two masks, different radii) and per-mask
Dehaze; hover the greyed mask-scope Dehaze Radius. Judge: does the radius-is-global story make
sense from the tooltip alone; is per-mask dehaze compounding (your accepted semantics)
something the UI should hint at anywhere?
Verdict: I think its fine as is. The effects are visible and an editor needs to know if what he is doing makes sense.

**3.6 MaskNone state.** Mask tool with no mask selected: faint banner + disabled controls with
"Create or select a mask first" hovers. Judge: does this state guide you to the Create button?
Verdict: I like.

**3.7 Per-scope disclosure memory.** Collapse sections in Mask scope, switch to Adjust and back.
Judge: is the independent memory helpful or surprising?
Verdict: Rather helpful.

## 4. Develop — canvas, tools, navigation

**4.1 Tool cluster & keybind hints.** The floating Adjust/Crop/Mask/Heal cluster + undo/redo.
Hover each — keybind hints present and correct? Judge placement and size at your window size.
Verdict: Looks good.

**4.2 Crop.** Crop tool: aspect chips, angle slider, keystone, Reset crop. Judge: handle grab
targets, the rule-of-thirds overlay, whether leaving Crop feels safe (no accidental commit).
Verdict: The Crop tool needs a major overhaul. Looking at the V2 Design a few things are still missing and it feels a bit cramed. I would also like to completly remove the tabs and like with masking display only the crop options. Then cropping itself does not work as expected. The aspect is not correctly kept when changing the crop. Also when applying the crop we have very funky artefacts like extruding the last pixel of the lower and right bounds outward to fill the space the original aspect used to take up. So this will need a single major pass. When exiting crop again no edits are displayed on the image. Also while cropping no edits are applied.

**4.3 Zoom/pan/compare.** 1:1 toggling, fit, pan feel at 1:1, Before/After (Y) split with the
divider. Judge responsiveness after all the engine work — call out ANY hitch that annoys.
Verdict: Only the issues I mentioned with the edited photo snapping back and forth between raw and edited.

**4.4 Filmstrip & navigation.** Arrow-key through 20 images incl. RAW+JPG mix. Judge: reveal
speed (warm cache), filmstrip thumbnail size/selected-state, whether the strip ever steals
attention.
Verdict: The only improvement I would like is to reduce the snappyness of the filmstrip. So a user can actually scroll the filmstrip and it does not snap directly back. Only when a user navigates to a new image (either keyboard action or with a click) the filmstrip snaps to that image centered at the top.

**4.5 Histogram & info.** The floating histogram, EXIF chip, Info panel toggle. Judge: histogram
size/legibility, whether the Info panel earns its left-side home.
Verdict: The floating info is fine. When the floating info overlay is not shown the info button should be moved to the bottom left instead of staying floating above the non-existent overlay. Then the left-hand info panel is not resizable. It always snaps back and the white line to resize on is never moving.

**4.6 Heal placeholder.** The Heal tool is visible but disabled. Judge: is a visible-but-disabled
future tool acceptable in the cluster, or should it hide until it exists?
Verdict: Lets keep it like that.

## 5. Export

**5.1 Queue flow.** Queue 5 mixed images from Library and Develop, set format/quality/resize,
pick a destination, export. Judge: the control-left/label-right convention (deliberately
reversed from Develop), the disabled-until-folder Start button, progress + completion feedback.
Verdict: All good.

**5.2 Settings comprehension.** Without docs: what does Effort do? Bit depth? Strip metadata vs
Copy EXIF? Judge label clarity alone.
Verdict: I judge it as fine.

## 6. Chrome, Settings, Help

**6.1 Titlebar & module nav.** Library/Develop/Export switching, logo/version, window controls.
Judge: active-module signal strength, menu discoverability.
Verdict: The titlebar is nearly good. in the V2 design there is an underline for the selected module. Please add that.

**6.2 Settings.** Open Settings: keyboard tab (every action listed, rebind one key and confirm
its tooltip updates), the gestures note, theme/layout options. Judge organization and whether
anything you've wanted to configure is missing.
Verdict: Settings looks fine. One thing could we align all shortcuts? Currently only per section is aligned.

**6.3 Help / shortcut discoverability.** Find the shortcut list from cold. Judge: would a new
user find Ctrl+scroll brush sizing, Y-split, M-overlay without being told?
Verdict: That is totally fine.

## 7. Anything else

Whatever bothered you that has no item above — write it here. Nothing is too small; "this 1px
line annoys me" is exactly the kind of thing Round 4 exists for.
Verdict: I think this is everything.
