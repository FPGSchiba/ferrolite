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
}

impl Action {
    /// All variants, for exhaustive iteration (defaults coverage + UI listing).
    pub const ALL: [Action; 20] = [
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
        }
    }
}

/// The subset of egui keys we bind. Serde-stable names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key {
    Escape,
    Enter,
    Backslash,
    Comma,
    F1,
    ArrowLeft,
    ArrowRight,
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    A,
    I,
    O,
    Q,
    Y,
    Z,
}

impl Key {
    pub fn to_egui(self) -> egui::Key {
        match self {
            Key::Escape => egui::Key::Escape,
            Key::Enter => egui::Key::Enter,
            Key::Backslash => egui::Key::Backslash,
            Key::Comma => egui::Key::Comma,
            Key::F1 => egui::Key::F1,
            Key::ArrowLeft => egui::Key::ArrowLeft,
            Key::ArrowRight => egui::Key::ArrowRight,
            Key::Num0 => egui::Key::Num0,
            Key::Num1 => egui::Key::Num1,
            Key::Num2 => egui::Key::Num2,
            Key::Num3 => egui::Key::Num3,
            Key::Num4 => egui::Key::Num4,
            Key::Num5 => egui::Key::Num5,
            Key::A => egui::Key::A,
            Key::I => egui::Key::I,
            Key::O => egui::Key::O,
            Key::Q => egui::Key::Q,
            Key::Y => egui::Key::Y,
            Key::Z => egui::Key::Z,
        }
    }

    /// Try to map an egui key we support (for capture during rebinding).
    pub fn from_egui(k: egui::Key) -> Option<Self> {
        Some(match k {
            egui::Key::Escape => Key::Escape,
            egui::Key::Enter => Key::Enter,
            egui::Key::Backslash => Key::Backslash,
            egui::Key::Comma => Key::Comma,
            egui::Key::F1 => Key::F1,
            egui::Key::ArrowLeft => Key::ArrowLeft,
            egui::Key::ArrowRight => Key::ArrowRight,
            egui::Key::Num0 => Key::Num0,
            egui::Key::Num1 => Key::Num1,
            egui::Key::Num2 => Key::Num2,
            egui::Key::Num3 => Key::Num3,
            egui::Key::Num4 => Key::Num4,
            egui::Key::Num5 => Key::Num5,
            egui::Key::A => Key::A,
            egui::Key::I => Key::I,
            egui::Key::O => Key::O,
            egui::Key::Q => Key::Q,
            egui::Key::Y => Key::Y,
            egui::Key::Z => Key::Z,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Key::Escape => "Esc",
            Key::Enter => "Enter",
            Key::Backslash => "\\",
            Key::Comma => ",",
            Key::F1 => "F1",
            Key::ArrowLeft => "←",
            Key::ArrowRight => "→",
            Key::Num0 => "0",
            Key::Num1 => "1",
            Key::Num2 => "2",
            Key::Num3 => "3",
            Key::Num4 => "4",
            Key::Num5 => "5",
            Key::A => "A",
            Key::I => "I",
            Key::O => "O",
            Key::Q => "Q",
            Key::Y => "Y",
            Key::Z => "Z",
        }
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
        s.push_str(self.key.label());
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
}
