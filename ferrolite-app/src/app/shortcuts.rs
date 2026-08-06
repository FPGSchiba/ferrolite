use crate::app::FerroliteApp;

/// Whether an `Action::Undo` keypress should route to the pending
/// batch-apply snapshot rather than Develop's per-image history undo.
///
/// True exactly when there is no active Develop session AND a batch-undo
/// snapshot is pending. Mirrors the gating `AppState::take_batch_undo`
/// (`state.rs`, called from `apply_undo_redo` in `app.rs`) itself applies
/// before consuming the snapshot, and the `can_undo` OR-condition computed
/// for the Edit menu — three call sites, one rule. This function only
/// decides whether it is worth calling `apply_undo_redo` at all; extracted
/// as a pure, egui-free predicate so THAT decision is unit-testable without
/// a `Context` (the Ctrl+Z chord match and `wants_keyboard_input` guard stay
/// in `dispatch` itself, which does need egui). `take_batch_undo` is the
/// one that actually owns the state transition (see its own tests in
/// `state.rs` for the take-exactly-once and Develop-session-blocks-it
/// invariants).
fn should_route_undo_to_batch(viewer_open: bool, batch_undo_pending: bool) -> bool {
    !viewer_open && batch_undo_pending
}

pub fn dispatch(ctx: &egui::Context, app: &mut FerroliteApp, frame: &mut eframe::Frame) {
    // Esc closes the viewer. Cancel its in-flight decode + tile jobs first so a
    // closed image's work stops competing with whatever is opened next.
    //
    // Precedence (spec C5 "leaving crop feels safe"): this dispatch runs
    // BEFORE the Develop canvas/tool rendering later in `app.rs`'s
    // `update()`, i.e. before `crop_overlay::show` gets a chance to see this
    // same Escape and cancel an in-progress handle drag. Without the
    // `crop_drag_active` guard below, Escape would close the WHOLE viewer
    // mid-drag before the drag-cancel code ever ran. Gated on the Crop tool
    // being active too (not just `drag_in_progress`) so a stale drag record
    // left behind by switching tools mid-drag (see `crop_overlay`'s module
    // doc comment) can't suppress `CloseViewer` while some other tool is
    // active. See `crop_overlay.rs`'s module doc comment for the full
    // precedence rationale.
    let crop_drag_active = app.state.tool_state.active == crate::develop::tool::ToolId::Crop
        && crate::develop::crop_overlay::drag_in_progress(ctx);
    if !crop_drag_active
        && app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::CloseViewer)
    {
        app.maybe_regen_on_leave(ctx, frame);
        if let Some(v) = app.state.viewer.take() {
            v.cancel_loads();
            app.cancel_viewer_tiles(frame, v.image_id);
            app.module = crate::module::Module::Library;
        }
    }

    // Enter opens the selected image in the viewer (library grid only, no
    // viewer already open, exactly one image selected). Suppressed while a
    // modal is up or a text field holds focus (so a future search box's
    // Enter won't pop the viewer).
    if app.module.is_library()
        && app.state.viewer.is_none()
        && !ctx.wants_keyboard_input()
        && app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::OpenImage)
    {
        if let Some(sel_id) = app.state.selected {
            if let Some(rec) = app.state.images.iter().find(|r| r.id == sel_id).cloned() {
                app.open_record(ctx, frame, &rec);
            }
        }
    }

    // F1 opens the Help modal. Global: works regardless of module/viewer
    // state, but suppressed while a text field holds focus or another
    // modal is up (consistent with the neighboring shortcuts here).
    if !ctx.wants_keyboard_input()
        && app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::OpenHelp)
    {
        app.show_help = true;
    }

    // Ctrl+, opens the Settings window. Global, same gating as Help
    // above. Since this whole region is gated on `!self.modal_active()`
    // (which now includes `show_settings`), the shortcut only opens
    // Settings when no modal is already up — acceptable, since a
    // modal already on screen has its own dismissal path.
    if !ctx.wants_keyboard_input()
        && app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::OpenSettings)
    {
        app.show_settings = true;
    }

    // Ctrl/Cmd+A toggles select-all over the current (filtered) grid rows.
    // Library grid only (no viewer, no modal, no text field focused).
    if app.module.is_library()
        && app.state.viewer.is_none()
        && !ctx.wants_keyboard_input()
        && app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::SelectAll)
    {
        app.state.toggle_select_all();
    }

    // P7: Ctrl+Z (Action::Undo, reused rather than a new binding — decision
    // P7-D5) also reverts a pending batch-apply snapshot when no Develop
    // session is open. Deliberately checked on `viewer.is_none()` alone,
    // with NO `app.module.is_library()` gate (unlike the select-all block
    // just above): the approved spec's condition is "no active Develop
    // session", full stop, matching the module-agnostic Edit menu's Undo
    // item (`chrome::MenuAction::Undo`, which also fires regardless of
    // module) — so this also fires in Export, not only the Library. Checked
    // independently of the Develop-only Undo/Redo block further down (which
    // requires `viewer.is_some()`, see the Left/Right navigation block
    // below), so the toast's "Press Ctrl+Z to undo." promise
    // (`presets::apply::batch_result_message`) is actually true wherever it
    // was raised from. Routes through the SAME `apply_undo_redo` funnel
    // Develop's own Ctrl+Z uses below — exactly one implementation, not two.
    // `apply_undo_redo` itself re-checks `viewer.is_none()` before touching
    // `batch_undo`, so even if this predicate's inputs were ever stale, a
    // Develop session cannot have its history undo hijacked by a pending
    // batch snapshot.
    if !ctx.wants_keyboard_input()
        && should_route_undo_to_batch(app.state.viewer.is_some(), app.state.batch_undo.is_some())
        && app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::Undo)
    {
        app.apply_undo_redo(ctx, frame, true);
    }

    // Keyboard metadata commands: rating 0–5 (I = Pick, O = Reject), all as
    // toggles. In Library (no viewer) they apply to the grid selection; in
    // Develop or Library+viewer they apply to the open viewer image.
    if !ctx.wants_keyboard_input() {
        use ferrolite_image::{Flag, Rating};

        // --- 1. Read key intent ---
        enum KeyIntent {
            Rating(u8),
            Flag(Flag),
        }
        // Routed through the keymap (one lookup per Action, each its own
        // `ctx.input` call inside `Keymap::pressed`); priority order (ratings
        // 0..5, then Pick, then Reject) and "one intent per frame" preserved.
        use crate::settings::keymap::Action;
        let km = &app.state.settings.keymap;
        let rating_actions = [
            Action::Rating0,
            Action::Rating1,
            Action::Rating2,
            Action::Rating3,
            Action::Rating4,
            Action::Rating5,
        ];
        let mut intent = None;
        for (n, action) in rating_actions.into_iter().enumerate() {
            if km.pressed(ctx, action) {
                intent = Some(KeyIntent::Rating(n as u8));
                break;
            }
        }
        let intent = intent.or_else(|| {
            if km.pressed(ctx, Action::FlagPick) {
                Some(KeyIntent::Flag(Flag::Pick))
            } else if km.pressed(ctx, Action::FlagReject) {
                Some(KeyIntent::Flag(Flag::Reject))
            } else {
                None
            }
        });

        if let Some(intent) = intent {
            // --- 2. Resolve target image id ---
            let target_id = if app.module.is_library() && app.state.viewer.is_none() {
                app.state.selected
            } else {
                app.state.viewer.as_ref().map(|v| v.image_id)
            };

            if let Some(target_id) = target_id {
                // --- 3. Look up current value ---
                let rec = app.state.images.iter().find(|r| r.id == target_id);
                let cur_rating = rec.map(|r| r.rating.get()).unwrap_or(0);
                let cur_flag = rec.map(|r| r.flag).unwrap_or(Flag::None);

                // --- 4. Build toggled edit ---
                let edit = match intent {
                    KeyIntent::Rating(n) => crate::metadata::MetaEdit::SetRating(Rating::new(
                        crate::metadata::toggle_rating(cur_rating, n),
                    )),
                    KeyIntent::Flag(f) => crate::metadata::MetaEdit::SetFlag(
                        crate::metadata::toggle_flag(cur_flag, f),
                    ),
                };

                // --- 5. Apply ---
                if app.module.is_library() && app.state.viewer.is_none() {
                    app.state.apply_metadata_edit(ctx, edit);
                } else {
                    app.state.apply_metadata_edit_to_image(ctx, target_id, edit);
                }
            }
        }

        // Q toggles export-queue membership for the same target image used
        // by the rating/flag intents above (grid selection in Library-no-
        // viewer, else the open viewer image). Kept as a parallel check
        // rather than folded into `KeyIntent` so the rating/flag toggle
        // logic above is untouched.
        if app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::AddToQueue)
        {
            let target_id = if app.module.is_library() && app.state.viewer.is_none() {
                app.state.selected
            } else {
                app.state.viewer.as_ref().map(|v| v.image_id)
            };
            if let Some(target_id) = target_id {
                let was_queued = app.state.queue_contains(target_id);
                app.state.queue_toggle(target_id);
                app.state.notify(
                    crate::notifications::Level::Info,
                    if was_queued {
                        "Removed from export queue."
                    } else {
                        "Added to export queue."
                    },
                );
            }
        }
    }

    // Left/Right move between images while viewing (Develop), non-cyclic.
    if app.module == crate::module::Module::Develop
        && app.state.viewer.is_some()
        && !ctx.wants_keyboard_input()
    {
        let km = &app.state.settings.keymap;
        let dir = if km.pressed(ctx, crate::settings::keymap::Action::NextImage) {
            Some(crate::viewer::nav::Step::Next)
        } else if km.pressed(ctx, crate::settings::keymap::Action::PrevImage) {
            Some(crate::viewer::nav::Step::Prev)
        } else {
            None
        };
        if let Some(dir) = dir {
            app.navigate_step(ctx, frame, dir);
        }

        // Before/After: `\` shows the empty (before) stack while held, and
        // reverts to the live stack on release.
        //
        // NOTE (Task 2.3 keymap routing, deliberate behavior change): the
        // dispatch for this refactor explicitly routes `HoldBeforePeek`
        // through `Keymap::held` (level-triggered), matching the keymap's
        // own design — `Action::HoldBeforePeek` is documented as "Hold to
        // show original (before)" and `held()` exists specifically for this
        // action. The pre-refactor code actually toggled `before_after` on
        // each `key_pressed` (an edge-triggered latch), which contradicted
        // its own doc comment in `viewer/mod.rs` calling it "momentary".
        // This routes it to the momentary/hold behavior the naming always
        // implied: `before_after` now directly mirrors "is the chord held",
        // only re-evaluating the preview on an actual state transition
        // (press or release), not every frame it's held.
        let hold_before = app
            .state
            .settings
            .keymap
            .held(ctx, crate::settings::keymap::Action::HoldBeforePeek);
        let before_after_changed = app
            .state
            .viewer
            .as_ref()
            .is_some_and(|v| v.before_after != hold_before);
        if before_after_changed {
            if let Some(v) = app.state.viewer.as_mut() {
                v.before_after = hold_before;
            }
            let stack = app.state.viewer.as_ref().unwrap().op_stack.clone();
            crate::app::controller::AppController::set_preview_and_full(app, frame, stack, true);
            // re-evaluates with before_after
        }

        // Undo / Redo. Redo also accepts the Ctrl+Y alias in addition to the
        // keymap's bound chord (defaults to Ctrl+Shift+Z) — kept for users
        // used to the common Ctrl+Y redo convention.
        let km = &app.state.settings.keymap;
        let undo = km.pressed(ctx, crate::settings::keymap::Action::Undo);
        let ctrl_y = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Y));
        let redo = km.pressed(ctx, crate::settings::keymap::Action::Redo) || ctrl_y;
        if undo || redo {
            app.apply_undo_redo(ctx, frame, undo);
        }

        // Toggle before/after SPLIT-compare (draggable divider), mirroring
        // the `develop_filter_bar` toggle button's click handling exactly:
        // flips `split_compare` and, only when turning it on, resets
        // `split_pos` to center. (Auto-fit-at-1:1 is a later task — not
        // added here.)
        if app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::ToggleSplitCompare)
        {
            app.toggle_split_compare();
        }

        // Tool-switch keybinds (A/C/M by default) and mask-overlay toggle.
        // Mirrors the tool palette's `SelectTool` handler's borrow
        // discipline: resolve `enabled` via a shared borrow first, then
        // take `&mut self.state.viewer` to apply it.
        let km = &app.state.settings.keymap;
        let tool = if km.pressed(ctx, crate::settings::keymap::Action::SwitchToolAdjust) {
            Some(crate::develop::tool::ToolId::Adjust)
        } else if km.pressed(ctx, crate::settings::keymap::Action::SwitchToolCrop) {
            Some(crate::develop::tool::ToolId::Crop)
        } else if km.pressed(ctx, crate::settings::keymap::Action::SwitchToolMask) {
            Some(crate::develop::tool::ToolId::Mask)
        } else {
            None
        };
        if let Some(id) = tool {
            let enabled = app
                .tool_registry
                .get(id)
                .map(|t| t.enabled(&crate::develop::tool::DevelopCtx { state: &app.state }))
                .unwrap_or(false);
            if app.state.viewer.is_some() {
                app.state
                    .tool_state
                    .select_tool(id, enabled, &app.tool_registry);
            }
        }

        if app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::ToggleMaskOverlay)
        {
            if let Some(v) = app.state.viewer.as_mut() {
                v.mask.overlay_on = !v.mask.overlay_on;
            }
        }

        // New Brush Layer (default `B`): starts a fresh, separately-deletable
        // `Brush` component in the selected mask — the explicit "split" for the
        // merge-by-default brush model. Gated on the Mask tool being active
        // (mirrors `ToggleMaskOverlay` above) plus an actual mask selection.
        if app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::NewBrushLayer)
        {
            let stack_and_idx = app.state.viewer.as_ref().and_then(|v| {
                (v.mask.active && v.mask.selected.is_some())
                    .then(|| (v.op_stack.clone(), v.mask.selected.unwrap()))
            });
            if let Some((stack, idx)) = stack_and_idx {
                let new_stack = crate::develop::mask_edit::new_brush_layer(&stack, idx);
                crate::app::controller::AppController::apply_edit(
                    app,
                    ctx,
                    frame,
                    ferrolite_pipeline::OpKind::LocalAdjustments,
                    new_stack,
                    true,
                );
            }
        }

        // Zoom-to-fit (default `F`) and Zoom 1:1 (default `Z`): rebuild the
        // same transforms the canvas's double-click toggle already builds
        // (`viewer/mod.rs`'s `paint`), just from a keybind instead of a
        // double-click gesture.
        if app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::ZoomFit)
        {
            if let Some(v) = app.state.viewer.as_mut() {
                if let Some(dims) = v.image_dims {
                    v.view = ferrolite_vt::ViewTransform::fit(dims, v.viewport);
                    v.idle = false;
                    // Snap cleanly to fit: drop any pending/residual scroll so
                    // trackpad momentum can't keep zooming past the fit this
                    // frame (drive_viewer reads these deltas later this frame).
                    ctx.input_mut(|i| {
                        i.raw_scroll_delta = egui::Vec2::ZERO;
                        i.smooth_scroll_delta = egui::Vec2::ZERO;
                    });
                    ctx.request_repaint();
                }
            }
        }
        if app
            .state
            .settings
            .keymap
            .pressed(ctx, crate::settings::keymap::Action::ZoomActual)
        {
            if let Some(v) = app.state.viewer.as_mut() {
                if v.image_dims.is_some() {
                    v.view = ferrolite_vt::ViewTransform {
                        zoom: 1.0,
                        pan: (0.0, 0.0),
                    };
                    v.idle = false;
                    // Same as fit: kill residual scroll velocity so the 1:1
                    // snap isn't immediately dragged off by trackpad momentum.
                    ctx.input_mut(|i| {
                        i.raw_scroll_delta = egui::Vec2::ZERO;
                        i.smooth_scroll_delta = egui::Vec2::ZERO;
                    });
                    ctx.request_repaint();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No Develop session open and a batch snapshot pending: Undo must route
    /// to the batch revert.
    #[test]
    fn routes_to_batch_when_no_viewer_and_snapshot_pending() {
        assert!(should_route_undo_to_batch(false, true));
    }

    /// No Develop session, but nothing to undo: must not route (there is no
    /// batch job for `apply_undo_redo`'s revert branch to act on).
    #[test]
    fn does_not_route_when_no_viewer_and_no_snapshot() {
        assert!(!should_route_undo_to_batch(false, false));
    }

    /// A Develop session IS open: Undo must never route to the batch
    /// snapshot even if one is pending — Develop's own history undo owns
    /// Ctrl+Z while a session is active. This is the inverse case the
    /// coordinator asked to double-check.
    #[test]
    fn never_routes_to_batch_while_a_develop_session_is_open() {
        assert!(!should_route_undo_to_batch(true, true));
        assert!(!should_route_undo_to_batch(true, false));
    }
}
