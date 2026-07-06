//! Central keymap: every bindable Action mapped to a Chord. All keyboard
//! shortcuts route through this so they can be remapped + persisted.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Every bindable command. Exhaustive: `Keymap::defaults()` binds all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Action {
    CloseViewer,
    OpenImage,
    SelectAll,
    Rating0,
    Rating1,
    Rating2,
    Rating3,
    Rating4,
    Rating5,
    FlagPick,
    FlagReject,
    AddToQueue,
    PrevImage,
    NextImage,
    HoldBeforePeek,
    ToggleSplitCompare,
    Undo,
    Redo,
    OpenSettings,
    OpenHelp,
    SwitchToolAdjust,
    SwitchToolCrop,
    SwitchToolMask,
    ToggleMaskOverlay,
}

impl Action {
    /// All variants, for exhaustive iteration (defaults coverage + UI listing).
    pub const ALL: [Action; 24] = [
        Action::CloseViewer,
        Action::OpenImage,
        Action::SelectAll,
        Action::Rating0,
        Action::Rating1,
        Action::Rating2,
        Action::Rating3,
        Action::Rating4,
        Action::Rating5,
        Action::FlagPick,
        Action::FlagReject,
        Action::AddToQueue,
        Action::PrevImage,
        Action::NextImage,
        Action::HoldBeforePeek,
        Action::ToggleSplitCompare,
        Action::Undo,
        Action::Redo,
        Action::OpenSettings,
        Action::OpenHelp,
        Action::SwitchToolAdjust,
        Action::SwitchToolCrop,
        Action::SwitchToolMask,
        Action::ToggleMaskOverlay,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::CloseViewer => "Close viewer / back to Library",
            Action::OpenImage => "Open selected image",
            Action::SelectAll => "Select all",
            Action::Rating0 => "Rating: 0 (clear)",
            Action::Rating1 => "Rating: 1",
            Action::Rating2 => "Rating: 2",
            Action::Rating3 => "Rating: 3",
            Action::Rating4 => "Rating: 4",
            Action::Rating5 => "Rating: 5",
            Action::FlagPick => "Flag: Pick",
            Action::FlagReject => "Flag: Reject",
            Action::AddToQueue => "Add to export queue",
            Action::PrevImage => "Previous image",
            Action::NextImage => "Next image",
            Action::HoldBeforePeek => "Hold to show original (before)",
            Action::ToggleSplitCompare => "Toggle before/after split",
            Action::Undo => "Undo",
            Action::Redo => "Redo",
            Action::OpenSettings => "Open Settings",
            Action::OpenHelp => "Open Help",
            Action::SwitchToolAdjust => "Tool: Adjust",
            Action::SwitchToolCrop => "Tool: Crop",
            Action::SwitchToolMask => "Tool: Mask",
            Action::ToggleMaskOverlay => "Toggle mask overlay",
        }
    }
}

/// A complete mirror of `egui::Key` (one variant per `egui::Key` variant,
/// using identical variant identifiers), so every key egui can report is
/// bindable. Serde-stable names: the variants that existed in the original
/// 19-key subset keep the same identifiers, so previously-persisted
/// `settings.json` files still deserialize (see
/// `legacy_key_variant_names_still_deserialize`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key {
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,

    Escape,
    Tab,
    Backspace,
    Enter,
    Space,

    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,

    Copy,
    Cut,
    Paste,

    Colon,
    Comma,
    Backslash,
    Slash,
    Pipe,
    Questionmark,
    OpenBracket,
    CloseBracket,
    Backtick,
    Minus,
    Period,
    Plus,
    Equals,
    Semicolon,
    Quote,

    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,

    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,

    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    F25,
    F26,
    F27,
    F28,
    F29,
    F30,
    F31,
    F32,
    F33,
    F34,
    F35,
}

impl Key {
    pub fn to_egui(self) -> egui::Key {
        match self {
            Key::ArrowDown => egui::Key::ArrowDown,
            Key::ArrowLeft => egui::Key::ArrowLeft,
            Key::ArrowRight => egui::Key::ArrowRight,
            Key::ArrowUp => egui::Key::ArrowUp,

            Key::Escape => egui::Key::Escape,
            Key::Tab => egui::Key::Tab,
            Key::Backspace => egui::Key::Backspace,
            Key::Enter => egui::Key::Enter,
            Key::Space => egui::Key::Space,

            Key::Insert => egui::Key::Insert,
            Key::Delete => egui::Key::Delete,
            Key::Home => egui::Key::Home,
            Key::End => egui::Key::End,
            Key::PageUp => egui::Key::PageUp,
            Key::PageDown => egui::Key::PageDown,

            Key::Copy => egui::Key::Copy,
            Key::Cut => egui::Key::Cut,
            Key::Paste => egui::Key::Paste,

            Key::Colon => egui::Key::Colon,
            Key::Comma => egui::Key::Comma,
            Key::Backslash => egui::Key::Backslash,
            Key::Slash => egui::Key::Slash,
            Key::Pipe => egui::Key::Pipe,
            Key::Questionmark => egui::Key::Questionmark,
            Key::OpenBracket => egui::Key::OpenBracket,
            Key::CloseBracket => egui::Key::CloseBracket,
            Key::Backtick => egui::Key::Backtick,
            Key::Minus => egui::Key::Minus,
            Key::Period => egui::Key::Period,
            Key::Plus => egui::Key::Plus,
            Key::Equals => egui::Key::Equals,
            Key::Semicolon => egui::Key::Semicolon,
            Key::Quote => egui::Key::Quote,

            Key::Num0 => egui::Key::Num0,
            Key::Num1 => egui::Key::Num1,
            Key::Num2 => egui::Key::Num2,
            Key::Num3 => egui::Key::Num3,
            Key::Num4 => egui::Key::Num4,
            Key::Num5 => egui::Key::Num5,
            Key::Num6 => egui::Key::Num6,
            Key::Num7 => egui::Key::Num7,
            Key::Num8 => egui::Key::Num8,
            Key::Num9 => egui::Key::Num9,

            Key::A => egui::Key::A,
            Key::B => egui::Key::B,
            Key::C => egui::Key::C,
            Key::D => egui::Key::D,
            Key::E => egui::Key::E,
            Key::F => egui::Key::F,
            Key::G => egui::Key::G,
            Key::H => egui::Key::H,
            Key::I => egui::Key::I,
            Key::J => egui::Key::J,
            Key::K => egui::Key::K,
            Key::L => egui::Key::L,
            Key::M => egui::Key::M,
            Key::N => egui::Key::N,
            Key::O => egui::Key::O,
            Key::P => egui::Key::P,
            Key::Q => egui::Key::Q,
            Key::R => egui::Key::R,
            Key::S => egui::Key::S,
            Key::T => egui::Key::T,
            Key::U => egui::Key::U,
            Key::V => egui::Key::V,
            Key::W => egui::Key::W,
            Key::X => egui::Key::X,
            Key::Y => egui::Key::Y,
            Key::Z => egui::Key::Z,

            Key::F1 => egui::Key::F1,
            Key::F2 => egui::Key::F2,
            Key::F3 => egui::Key::F3,
            Key::F4 => egui::Key::F4,
            Key::F5 => egui::Key::F5,
            Key::F6 => egui::Key::F6,
            Key::F7 => egui::Key::F7,
            Key::F8 => egui::Key::F8,
            Key::F9 => egui::Key::F9,
            Key::F10 => egui::Key::F10,
            Key::F11 => egui::Key::F11,
            Key::F12 => egui::Key::F12,
            Key::F13 => egui::Key::F13,
            Key::F14 => egui::Key::F14,
            Key::F15 => egui::Key::F15,
            Key::F16 => egui::Key::F16,
            Key::F17 => egui::Key::F17,
            Key::F18 => egui::Key::F18,
            Key::F19 => egui::Key::F19,
            Key::F20 => egui::Key::F20,
            Key::F21 => egui::Key::F21,
            Key::F22 => egui::Key::F22,
            Key::F23 => egui::Key::F23,
            Key::F24 => egui::Key::F24,
            Key::F25 => egui::Key::F25,
            Key::F26 => egui::Key::F26,
            Key::F27 => egui::Key::F27,
            Key::F28 => egui::Key::F28,
            Key::F29 => egui::Key::F29,
            Key::F30 => egui::Key::F30,
            Key::F31 => egui::Key::F31,
            Key::F32 => egui::Key::F32,
            Key::F33 => egui::Key::F33,
            Key::F34 => egui::Key::F34,
            Key::F35 => egui::Key::F35,
        }
    }

    /// Map an egui key to ours (for capture during rebinding). `egui::Key` is
    /// a plain (non-`#[non_exhaustive]`) enum as of egui 0.29, and every
    /// current `egui::Key::ALL` variant maps to `Some` (enforced by
    /// `every_egui_key_is_bindable_and_roundtrips`); the `Option` return is
    /// kept for forward-compat with future egui versions adding keys, and
    /// because the capture call site in `settings/ui/keyboard.rs` already
    /// expects `Option`.
    pub fn from_egui(k: egui::Key) -> Option<Self> {
        Some(match k {
            egui::Key::ArrowDown => Key::ArrowDown,
            egui::Key::ArrowLeft => Key::ArrowLeft,
            egui::Key::ArrowRight => Key::ArrowRight,
            egui::Key::ArrowUp => Key::ArrowUp,

            egui::Key::Escape => Key::Escape,
            egui::Key::Tab => Key::Tab,
            egui::Key::Backspace => Key::Backspace,
            egui::Key::Enter => Key::Enter,
            egui::Key::Space => Key::Space,

            egui::Key::Insert => Key::Insert,
            egui::Key::Delete => Key::Delete,
            egui::Key::Home => Key::Home,
            egui::Key::End => Key::End,
            egui::Key::PageUp => Key::PageUp,
            egui::Key::PageDown => Key::PageDown,

            egui::Key::Copy => Key::Copy,
            egui::Key::Cut => Key::Cut,
            egui::Key::Paste => Key::Paste,

            egui::Key::Colon => Key::Colon,
            egui::Key::Comma => Key::Comma,
            egui::Key::Backslash => Key::Backslash,
            egui::Key::Slash => Key::Slash,
            egui::Key::Pipe => Key::Pipe,
            egui::Key::Questionmark => Key::Questionmark,
            egui::Key::OpenBracket => Key::OpenBracket,
            egui::Key::CloseBracket => Key::CloseBracket,
            egui::Key::Backtick => Key::Backtick,
            egui::Key::Minus => Key::Minus,
            egui::Key::Period => Key::Period,
            egui::Key::Plus => Key::Plus,
            egui::Key::Equals => Key::Equals,
            egui::Key::Semicolon => Key::Semicolon,
            egui::Key::Quote => Key::Quote,

            egui::Key::Num0 => Key::Num0,
            egui::Key::Num1 => Key::Num1,
            egui::Key::Num2 => Key::Num2,
            egui::Key::Num3 => Key::Num3,
            egui::Key::Num4 => Key::Num4,
            egui::Key::Num5 => Key::Num5,
            egui::Key::Num6 => Key::Num6,
            egui::Key::Num7 => Key::Num7,
            egui::Key::Num8 => Key::Num8,
            egui::Key::Num9 => Key::Num9,

            egui::Key::A => Key::A,
            egui::Key::B => Key::B,
            egui::Key::C => Key::C,
            egui::Key::D => Key::D,
            egui::Key::E => Key::E,
            egui::Key::F => Key::F,
            egui::Key::G => Key::G,
            egui::Key::H => Key::H,
            egui::Key::I => Key::I,
            egui::Key::J => Key::J,
            egui::Key::K => Key::K,
            egui::Key::L => Key::L,
            egui::Key::M => Key::M,
            egui::Key::N => Key::N,
            egui::Key::O => Key::O,
            egui::Key::P => Key::P,
            egui::Key::Q => Key::Q,
            egui::Key::R => Key::R,
            egui::Key::S => Key::S,
            egui::Key::T => Key::T,
            egui::Key::U => Key::U,
            egui::Key::V => Key::V,
            egui::Key::W => Key::W,
            egui::Key::X => Key::X,
            egui::Key::Y => Key::Y,
            egui::Key::Z => Key::Z,

            egui::Key::F1 => Key::F1,
            egui::Key::F2 => Key::F2,
            egui::Key::F3 => Key::F3,
            egui::Key::F4 => Key::F4,
            egui::Key::F5 => Key::F5,
            egui::Key::F6 => Key::F6,
            egui::Key::F7 => Key::F7,
            egui::Key::F8 => Key::F8,
            egui::Key::F9 => Key::F9,
            egui::Key::F10 => Key::F10,
            egui::Key::F11 => Key::F11,
            egui::Key::F12 => Key::F12,
            egui::Key::F13 => Key::F13,
            egui::Key::F14 => Key::F14,
            egui::Key::F15 => Key::F15,
            egui::Key::F16 => Key::F16,
            egui::Key::F17 => Key::F17,
            egui::Key::F18 => Key::F18,
            egui::Key::F19 => Key::F19,
            egui::Key::F20 => Key::F20,
            egui::Key::F21 => Key::F21,
            egui::Key::F22 => Key::F22,
            egui::Key::F23 => Key::F23,
            egui::Key::F24 => Key::F24,
            egui::Key::F25 => Key::F25,
            egui::Key::F26 => Key::F26,
            egui::Key::F27 => Key::F27,
            egui::Key::F28 => Key::F28,
            egui::Key::F29 => Key::F29,
            egui::Key::F30 => Key::F30,
            egui::Key::F31 => Key::F31,
            egui::Key::F32 => Key::F32,
            egui::Key::F33 => Key::F33,
            egui::Key::F34 => Key::F34,
            egui::Key::F35 => Key::F35,
        })
    }

    /// Readable label for this key, delegating to egui's own naming so every
    /// mirrored variant gets a sensible display string (symbol where egui has
    /// one, e.g. "⏴" for ArrowLeft, else its English name, e.g. "F5").
    pub fn label(self) -> String {
        self.to_egui().symbol_or_name().to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chord {
    pub key: Key,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

impl Chord {
    pub fn label(self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        s.push_str(&self.key.label());
        s
    }
}

/// `Action → Chord` bindings. Every action must have a binding at all times
/// (see `Keymap::defaults()` and the fill-missing pass); edits must call
/// `mark_settings_dirty()` on the owning `AppState` so they persist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keymap {
    map: BTreeMap<Action, Chord>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::defaults()
    }
}

fn plain(key: Key) -> Chord {
    Chord {
        key,
        ctrl: false,
        shift: false,
        alt: false,
    }
}
fn ctrl(key: Key) -> Chord {
    Chord {
        key,
        ctrl: true,
        shift: false,
        alt: false,
    }
}
fn ctrl_shift(key: Key) -> Chord {
    Chord {
        key,
        ctrl: true,
        shift: true,
        alt: false,
    }
}

impl Keymap {
    pub fn defaults() -> Self {
        use Action::*;
        let mut m = BTreeMap::new();
        m.insert(CloseViewer, plain(Key::Escape));
        m.insert(OpenImage, plain(Key::Enter));
        m.insert(SelectAll, ctrl(Key::A));
        m.insert(Rating0, plain(Key::Num0));
        m.insert(Rating1, plain(Key::Num1));
        m.insert(Rating2, plain(Key::Num2));
        m.insert(Rating3, plain(Key::Num3));
        m.insert(Rating4, plain(Key::Num4));
        m.insert(Rating5, plain(Key::Num5));
        m.insert(FlagPick, plain(Key::I));
        m.insert(FlagReject, plain(Key::O));
        m.insert(AddToQueue, plain(Key::Q));
        m.insert(PrevImage, plain(Key::ArrowLeft));
        m.insert(NextImage, plain(Key::ArrowRight));
        m.insert(HoldBeforePeek, plain(Key::Backslash));
        m.insert(ToggleSplitCompare, plain(Key::Y));
        m.insert(Undo, ctrl(Key::Z));
        m.insert(Redo, ctrl_shift(Key::Z));
        m.insert(OpenSettings, ctrl(Key::Comma));
        m.insert(OpenHelp, plain(Key::F1));
        m.insert(SwitchToolAdjust, plain(Key::A));
        m.insert(SwitchToolCrop, plain(Key::C));
        m.insert(SwitchToolMask, plain(Key::M));
        // `O` is FlagReject's default (see above) — `T` ("toggle") is free.
        m.insert(ToggleMaskOverlay, plain(Key::T));
        // Fill any missing action (forward-compat) with a harmless default.
        for a in Action::ALL {
            m.entry(a).or_insert(plain(Key::F1));
        }
        Self { map: m }
    }

    pub fn chord(&self, a: Action) -> Chord {
        *self.map.get(&a).unwrap_or(&plain(Key::F1))
    }

    pub fn set(&mut self, a: Action, c: Chord) {
        self.map.insert(a, c);
    }
    pub fn reset(&mut self, a: Action) {
        self.map.insert(a, Keymap::defaults().chord(a));
    }

    /// The other action already bound to `c`, if any (ignoring `a` itself).
    pub fn conflict(&self, a: Action, c: Chord) -> Option<Action> {
        self.map
            .iter()
            .find(|(other, ch)| **other != a && **ch == c)
            .map(|(o, _)| *o)
    }

    /// Fill any action missing from `map` with its default chord. Guards
    /// against a hand-edited/partial settings file (e.g. an explicit `{}`
    /// keymap object) deserializing to an incomplete map, which would
    /// otherwise collapse every unbound action's `chord()` lookup to the
    /// shared F1 fallback.
    pub fn ensure_complete(&mut self) {
        for a in Action::ALL {
            self.map
                .entry(a)
                .or_insert_with(|| Keymap::defaults().chord(a));
        }
    }

    fn matches(ctx: &egui::Context, c: Chord) -> (bool, bool) {
        ctx.input(|i| {
            let mods = i.modifiers.command == c.ctrl
                && i.modifiers.shift == c.shift
                && i.modifiers.alt == c.alt;
            (
                mods && i.key_pressed(c.key.to_egui()),
                mods && i.key_down(c.key.to_egui()),
            )
        })
    }

    /// Edge-triggered: the chord was pressed this frame.
    pub fn pressed(&self, ctx: &egui::Context, a: Action) -> bool {
        Self::matches(ctx, self.chord(a)).0
    }

    /// Level-triggered: the chord's key is currently held (for HoldBeforePeek).
    pub fn held(&self, ctx: &egui::Context, a: Action) -> bool {
        Self::matches(ctx, self.chord(a)).1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_label_formats_mods_then_key() {
        let c = Chord {
            key: Key::Z,
            ctrl: true,
            shift: true,
            alt: false,
        };
        assert_eq!(c.label(), "Ctrl+Shift+Z");
        let q = Chord {
            key: Key::Q,
            ctrl: false,
            shift: false,
            alt: false,
        };
        assert_eq!(q.label(), "Q");
    }

    #[test]
    fn chord_serde_roundtrip() {
        let c = Chord {
            key: Key::Comma,
            ctrl: true,
            shift: false,
            alt: false,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Chord>(&s).unwrap(), c);
    }

    #[test]
    fn defaults_bind_every_action() {
        let km = Keymap::defaults();
        for a in Action::ALL {
            assert!(km.map.contains_key(&a), "missing default for {:?}", a);
        }
    }

    #[test]
    fn defaults_match_documented_bindings() {
        let km = Keymap::defaults();
        assert_eq!(
            km.chord(Action::AddToQueue),
            Chord {
                key: Key::Q,
                ctrl: false,
                shift: false,
                alt: false
            }
        );
        assert_eq!(
            km.chord(Action::SelectAll),
            Chord {
                key: Key::A,
                ctrl: true,
                shift: false,
                alt: false
            }
        );
        assert_eq!(
            km.chord(Action::Redo),
            Chord {
                key: Key::Z,
                ctrl: true,
                shift: true,
                alt: false
            }
        );
        assert_eq!(
            km.chord(Action::ToggleSplitCompare),
            Chord {
                key: Key::Y,
                ctrl: false,
                shift: false,
                alt: false
            }
        );
        assert_eq!(
            km.chord(Action::OpenSettings),
            Chord {
                key: Key::Comma,
                ctrl: true,
                shift: false,
                alt: false
            }
        );
    }

    #[test]
    fn conflict_detects_duplicate_chord() {
        let km = Keymap::defaults();
        // Q is AddToQueue by default; assigning Q to Rating1 conflicts.
        let q = Chord {
            key: Key::Q,
            ctrl: false,
            shift: false,
            alt: false,
        };
        assert_eq!(km.conflict(Action::Rating1, q), Some(Action::AddToQueue));
        // Assigning a chord to the SAME action it already holds is not a conflict.
        assert_eq!(km.conflict(Action::AddToQueue, q), None);
        // A free chord conflicts with nothing.
        let free = Chord {
            key: Key::F1,
            ctrl: true,
            shift: true,
            alt: true,
        };
        assert_eq!(km.conflict(Action::Rating1, free), None);
    }

    #[test]
    fn set_and_reset_roundtrip() {
        let mut km = Keymap::defaults();
        let new = Chord {
            key: Key::Y,
            ctrl: true,
            shift: false,
            alt: false,
        };
        km.set(Action::ToggleSplitCompare, new);
        assert_eq!(km.chord(Action::ToggleSplitCompare), new);
        km.reset(Action::ToggleSplitCompare);
        assert_eq!(
            km.chord(Action::ToggleSplitCompare),
            Keymap::defaults().chord(Action::ToggleSplitCompare)
        );
    }

    #[test]
    fn every_egui_key_is_bindable_and_roundtrips() {
        // Rebinding must accept ANY key, not just the default-binding subset.
        for &k in egui::Key::ALL {
            let ours = Key::from_egui(k);
            assert!(
                ours.is_some(),
                "egui::Key::{k:?} must be bindable (from_egui returned None)"
            );
            assert_eq!(ours.unwrap().to_egui(), k, "round-trip failed for {k:?}");
        }
    }

    #[test]
    fn new_develop_actions_have_defaults_and_no_internal_conflict() {
        let km = Keymap::defaults();
        use Action::*;
        // Every action (incl. the new ones) is bound — the existing exhaustiveness
        // test already covers this via Action::ALL; here assert the new ones resolve.
        for a in [
            SwitchToolAdjust,
            SwitchToolCrop,
            SwitchToolMask,
            ToggleMaskOverlay,
        ] {
            let _ = km.chord(a); // must not panic / must be present
        }
        // The new defaults must not collide with each other.
        let news = [
            SwitchToolAdjust,
            SwitchToolCrop,
            SwitchToolMask,
            ToggleMaskOverlay,
        ];
        for &a in &news {
            if let Some(other) = km.conflict(a, km.chord(a)) {
                assert!(
                    !news.contains(&other) || other == a,
                    "new action {a:?} conflicts with {other:?}"
                );
            }
        }
    }

    #[test]
    fn toggle_mask_overlay_does_not_collide_with_flag_reject() {
        // `O` is FlagReject's default; ToggleMaskOverlay must use a different
        // key so the two don't collide despite living in different contexts.
        let km = Keymap::defaults();
        assert_eq!(
            km.conflict(
                Action::ToggleMaskOverlay,
                km.chord(Action::ToggleMaskOverlay)
            ),
            None
        );
        assert_ne!(
            km.chord(Action::ToggleMaskOverlay).key,
            Key::O,
            "ToggleMaskOverlay must not default to O (FlagReject's key)"
        );
    }

    #[test]
    fn legacy_key_variant_names_still_deserialize() {
        // Existing settings.json stored the OLD enum variant names — they must still load.
        for name in [
            "ArrowLeft",
            "ArrowRight",
            "Num0",
            "Num1",
            "Num5",
            "Q",
            "Backslash",
            "Comma",
            "Escape",
            "Enter",
            "F1",
            "A",
            "I",
            "O",
            "Y",
            "Z",
        ] {
            let json = format!("\"{name}\"");
            assert!(
                serde_json::from_str::<Key>(&json).is_ok(),
                "legacy key name {name} must deserialize"
            );
        }
    }
}
