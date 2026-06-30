//! Library metadata value types: rating, flag, tag colour, tag id.

/// Star rating, clamped to 0..=5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rating(u8);

impl Rating {
    pub fn new(v: u8) -> Self {
        Self(v.min(5))
    }
    pub fn get(self) -> u8 {
        self.0
    }
    pub fn as_i64(self) -> i64 {
        self.0 as i64
    }
    pub fn from_i64(v: i64) -> Self {
        Self::new(v.clamp(0, 5) as u8)
    }
}

/// Pick / reject cull flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flag {
    #[default]
    None,
    Pick,
    Reject,
}

impl Flag {
    pub fn as_i64(self) -> i64 {
        match self {
            Flag::None => 0,
            Flag::Pick => 1,
            Flag::Reject => 2,
        }
    }
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => Flag::Pick,
            2 => Flag::Reject,
            _ => Flag::None,
        }
    }
}

/// An sRGB tag colour, stored packed as `0x00RRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Default for Color {
    fn default() -> Self {
        Color {
            r: 0x80,
            g: 0x80,
            b: 0x80,
        }
    }
}

impl Color {
    pub fn from_packed(v: u32) -> Self {
        Color {
            r: ((v >> 16) & 0xFF) as u8,
            g: ((v >> 8) & 0xFF) as u8,
            b: (v & 0xFF) as u8,
        }
    }
    pub fn to_packed(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
    pub fn from_hex(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#').unwrap_or(s);
        if h.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Color::from_packed(v))
    }
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

/// Stable tag identity (SQLite `tags.id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TagId(pub i64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rating_saturates_at_five() {
        assert_eq!(Rating::new(9).get(), 5);
        assert_eq!(Rating::new(3).get(), 3);
        assert_eq!(Rating::from_i64(-2).get(), 0);
        assert_eq!(Rating::from_i64(7).get(), 5);
        assert_eq!(Rating::default().get(), 0);
    }

    #[test]
    fn flag_round_trips_through_i64() {
        for f in [Flag::None, Flag::Pick, Flag::Reject] {
            assert_eq!(Flag::from_i64(f.as_i64()), f);
        }
        assert_eq!(Flag::from_i64(99), Flag::None);
        assert_eq!(Flag::default(), Flag::None);
    }

    #[test]
    fn color_packs_and_parses_hex() {
        let c = Color {
            r: 0xE5,
            g: 0x48,
            b: 0x4D,
        };
        assert_eq!(c.to_packed(), 0x00E5_484D);
        assert_eq!(Color::from_packed(0x00E5_484D), c);
        assert_eq!(c.to_hex(), "#E5484D");
        assert_eq!(Color::from_hex("#E5484D"), Some(c));
        assert_eq!(Color::from_hex("E5484D"), Some(c));
        assert_eq!(Color::from_hex("nope"), None);
    }
}
