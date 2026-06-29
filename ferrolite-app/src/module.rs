//! Top-level UI module selection (Library vs Develop).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Module {
    #[default]
    Library,
    Develop,
}

impl Module {
    pub fn is_library(self) -> bool {
        matches!(self, Module::Library)
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
}
