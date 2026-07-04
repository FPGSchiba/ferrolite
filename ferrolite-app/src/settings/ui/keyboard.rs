//! The Settings window's Keyboard tab: lists every bindable `Action` grouped
//! like the Help modal (`ferrolite-app/src/help.rs`), lets the user click a
//! row's chord button to rebind it, and gives each row + the whole map its
//! own reset affordance (per-control reset is a load-bearing CLAUDE.md rule).

use super::Settings;
use crate::settings::keymap::{Action, Chord, Key, Keymap};
use crate::theme;
use crate::widgets::draw_reset_arrow;

/// Same grouping as the Help modal's `GROUPS` table
/// (`ferrolite-app/src/help.rs`), replicated here so this tab's layout
/// matches the reference list exactly. Kept as a separate flat table (rather
/// than sharing `help::GROUPS`, which is private) — the two are covered by
/// the plan's "mirror or replicate" instruction.
const GROUPS: &[(&str, &[Action])] = &[
    (
        "Navigation",
        &[
            Action::CloseViewer,
            Action::OpenImage,
            Action::SelectAll,
            Action::PrevImage,
            Action::NextImage,
        ],
    ),
    (
        "Rating & Flags",
        &[
            Action::Rating0,
            Action::Rating1,
            Action::Rating2,
            Action::Rating3,
            Action::Rating4,
            Action::Rating5,
            Action::FlagPick,
            Action::FlagReject,
        ],
    ),
    (
        "Develop",
        &[
            Action::HoldBeforePeek,
            Action::ToggleSplitCompare,
            Action::AddToQueue,
        ],
    ),
    ("Editing", &[Action::Undo, Action::Redo]),
    ("App", &[Action::OpenSettings, Action::OpenHelp]),
];

/// The action currently being rebound (listening for its next keypress), if
/// any, stored in egui memory. Also carries an inline conflict message to
/// show under that row until the user presses a different, non-conflicting
/// key or cancels with Esc.
#[derive(Debug, Clone, PartialEq, Default)]
struct ListenState {
    action: Option<Action>,
    conflict: Option<Action>,
}

fn listen_id() -> egui::Id {
    egui::Id::new("settings_keymap_listening")
}

fn listen_state(ctx: &egui::Context) -> ListenState {
    ctx.data(|d| d.get_temp(listen_id())).unwrap_or_default()
}

fn set_listen_state(ctx: &egui::Context, state: ListenState) {
    ctx.data_mut(|d| d.insert_temp(listen_id(), state));
}

/// Draw the Keyboard tab. Returns `true` if any binding changed this frame.
pub(super) fn draw(ui: &mut egui::Ui, settings: &mut Settings) -> bool {
    let ctx = ui.ctx().clone();
    let mut changed = false;
    let mut listen = listen_state(&ctx);

    ui.heading("Keyboard");
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "Click a shortcut to rebind it, then press the new key combination. Esc cancels.",
        )
        .color(theme::TEXT_DIM)
        .size(11.0),
    );
    ui.add_space(8.0);

    if ui.button("Reset all shortcuts").clicked() {
        settings.keymap = Keymap::defaults();
        listen = ListenState::default();
        changed = true;
    }
    ui.add_space(10.0);

    // While listening, capture the next mappable keypress BEFORE drawing the
    // rows (so this frame's row already reflects any resulting change/cancel).
    if let Some(action) = listen.action {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            // Bare Esc cancels listening rather than binding to Escape. Esc
            // is otherwise a valid bindable `Key::Escape` (e.g. CloseViewer's
            // default), but overloading it as "cancel capture" is the more
            // useful/expected behavior for a rebind UI, and a user who really
            // wants to bind Esc to something else has no other way to exit
            // capture mode. Documented deliberate choice (see dispatch report).
            //
            // Consumed (rather than a non-consuming `key_pressed` peek) so the
            // Settings window's own Esc-to-close check in `ui.rs::show` (which
            // runs after this tab is drawn) doesn't also see this same Esc
            // press and close the whole dialog in the same frame.
            listen = ListenState::default();
        } else {
            let mods = ctx.input(|i| i.modifiers);
            let pressed_key = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        ..
                    } => Key::from_egui(*key),
                    _ => None,
                })
            });
            if let Some(key) = pressed_key {
                let chord = Chord {
                    key,
                    ctrl: mods.command,
                    shift: mods.shift,
                    alt: mods.alt,
                };
                match settings.keymap.conflict(action, chord) {
                    Some(other) => {
                        listen.conflict = Some(other);
                    }
                    None => {
                        settings.keymap.set(action, chord);
                        changed = true;
                        listen = ListenState::default();
                    }
                }
            }
            // Unsupported keys (no `Key::from_egui` mapping) fall through:
            // `pressed_key` is `None`, so we simply keep listening.
        }
    }

    // This tab's content is drawn inside `settings::ui`'s vertical
    // `settings_content_scroll` ScrollArea. Its floating scrollbar overlays
    // the right edge, on top of this grid's rightmost columns (the chord
    // button and reset arrow) unless we reserve room for it — so the whole
    // per-group grid is inset from the right by the scrollbar's width.
    egui::Frame::none()
        .inner_margin(egui::Margin {
            left: 0.0,
            right: 16.0,
            top: 0.0,
            bottom: 0.0,
        })
        .show(ui, |ui| {
            for (group_name, actions) in GROUPS {
                ui.label(
                    egui::RichText::new(*group_name)
                        .strong()
                        .color(theme::ACCENT),
                );
                ui.add_space(2.0);
                egui::Grid::new(("settings_keymap_grid", *group_name))
                    .num_columns(4)
                    .spacing(egui::vec2(12.0, 6.0))
                    .striped(false)
                    .show(ui, |ui| {
                        for action in *actions {
                            draw_row(ui, settings, *action, &mut listen, &mut changed);
                            ui.end_row();
                        }
                    });
                ui.add_space(10.0);
            }
        });

    set_listen_state(&ctx, listen);
    changed
}

fn draw_row(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    action: Action,
    listen: &mut ListenState,
    changed: &mut bool,
) {
    let is_listening = listen.action == Some(action);
    let default_chord = Keymap::defaults().chord(action);
    let current_chord = settings.keymap.chord(action);
    let is_modified = current_chord != default_chord;

    ui.label(action.label());

    let button_label = if is_listening {
        "Press a key…".to_string()
    } else {
        current_chord.label()
    };
    let btn = ui.add(egui::Button::new(
        egui::RichText::new(button_label).monospace(),
    ));
    if btn.clicked() {
        *listen = ListenState {
            action: Some(action),
            conflict: None,
        };
    }

    // Per-row reset arrow: only interactive/visible-as-modified when this
    // action's chord differs from its default (mirrors `EguiSlider`'s
    // modified-dims-when-unchanged treatment).
    let (reset_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    // `Action` doesn't derive `Hash` (not needed for its `BTreeMap<Action, _>`
    // key use elsewhere), so salt the id with its unique label string instead
    // of the enum value itself.
    let reset_resp = ui.interact(
        reset_rect,
        ui.id().with(("keymap_reset", action.label())),
        egui::Sense::click(),
    );
    let reset_color = if is_modified {
        if reset_resp.hovered() {
            theme::ACCENT_BRIGHT
        } else {
            theme::TEXT_DIM
        }
    } else {
        theme::BORDER_STRONG
    };
    draw_reset_arrow(ui.painter(), reset_rect.center(), 6.0, reset_color);
    if reset_resp.clicked() && is_modified {
        settings.keymap.reset(action);
        *changed = true;
        if is_listening {
            *listen = ListenState::default();
        }
    }

    // Fourth column: inline conflict warning while listening for this row's
    // action, empty otherwise. Kept as a same-row grid cell (rather than an
    // extra `ui.end_row()`) since inserting a row mid-iteration would corrupt
    // the shared `egui::Grid`'s column layout for all other rows.
    if is_listening {
        if let Some(other) = listen.conflict {
            ui.colored_label(
                theme::SEMANTIC_RED,
                format!("Already bound to {}", other.label()),
            );
        } else {
            ui.label("");
        }
    } else {
        ui.label("");
    }
}
