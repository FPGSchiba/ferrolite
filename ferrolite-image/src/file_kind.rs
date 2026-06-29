//! RAW-vs-standard classification of an ingested image. Persisted in the
//! catalog (`images.kind`) so consumers route decode without re-inferring.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Raw,
    Standard,
}

impl FileKind {
    pub fn as_i64(self) -> i64 {
        match self {
            FileKind::Raw => 0,
            FileKind::Standard => 1,
        }
    }

    pub fn from_i64(v: i64) -> FileKind {
        match v {
            1 => FileKind::Standard,
            _ => FileKind::Raw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_i64() {
        assert_eq!(FileKind::from_i64(FileKind::Raw.as_i64()), FileKind::Raw);
        assert_eq!(
            FileKind::from_i64(FileKind::Standard.as_i64()),
            FileKind::Standard
        );
    }

    #[test]
    fn unknown_i64_defaults_to_raw() {
        assert_eq!(FileKind::from_i64(0), FileKind::Raw);
        assert_eq!(FileKind::from_i64(99), FileKind::Raw);
    }
}
