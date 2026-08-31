//! Top-level UI module selection (Library, Develop, Export).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Module {
    #[default]
    Library,
    Develop,
    Export,
}

impl Module {
    pub fn is_library(self) -> bool {
        matches!(self, Module::Library)
    }

    /// True while a Develop session is the active module — the module-level
    /// signal for "is there an active Develop session" gates use (P7 batch
    /// undo routing: `AppState::take_batch_undo`, `app::shortcuts`,
    /// `FerroliteApp`'s `can_undo`). Deliberately NOT `AppState.viewer.is_some()`:
    /// switching module tabs away from Develop back to Library does not clear
    /// `viewer` (it stays populated so returning to Develop resumes the same
    /// image), so `viewer.is_some()` stays true long after the user has left
    /// Develop and is an unreliable proxy for "a Develop session is active".
    pub fn is_develop(self) -> bool {
        matches!(self, Module::Develop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_library() {
        assert_eq!(Module::default(), Module::Library);
        assert!(Module::default().is_library());
    }

    #[test]
    fn develop_is_not_library() {
        assert!(!Module::Develop.is_library());
    }

    #[test]
    fn only_develop_is_develop() {
        assert!(Module::Develop.is_develop());
        assert!(!Module::Library.is_develop());
        assert!(!Module::Export.is_develop());
    }
}
