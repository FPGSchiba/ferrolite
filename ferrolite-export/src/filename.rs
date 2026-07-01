//! Pure filename token expansion + collision resolution for batch export
//! (spec §8.4). No filesystem or clock access — every value is supplied by the
//! caller so the whole module is unit-testable on every OS.

use std::collections::HashSet;

/// Values substituted into a filename template.
#[derive(Debug, Clone)]
pub struct FilenameCtx {
    /// Original file basename (no extension), for `{name}`.
    pub name: String,
    /// 1-based counter for `{seq}` / `{seq:0N}`.
    pub seq: usize,
    /// Preformatted date string for `{date}` (e.g. "2026-06-29"); may be empty.
    pub date: String,
    /// Camera make+model for `{camera}`; may be empty.
    pub camera: String,
}

/// Expand `template` against `ctx`. Recognised tokens: `{name}`, `{seq}`,
/// `{seq:0N}` (zero-padded to width N), `{date}`, `{camera}`. Any other
/// `{...}` run is emitted verbatim (braces included). Literal text passes through.
pub fn expand(template: &str, ctx: &FilenameCtx) -> String {
    let mut out = String::with_capacity(template.len() + 8);
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(close) = template[i..].find('}') {
                let token = &template[i + 1..i + close];
                match resolve_token(token, ctx) {
                    Some(rep) => {
                        out.push_str(&rep);
                        i += close + 1;
                        continue;
                    }
                    None => {
                        // Unknown token → emit verbatim including braces.
                        out.push_str(&template[i..i + close + 1]);
                        i += close + 1;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn resolve_token(token: &str, ctx: &FilenameCtx) -> Option<String> {
    match token {
        "name" => Some(ctx.name.clone()),
        "seq" => Some(ctx.seq.to_string()),
        "date" => Some(ctx.date.clone()),
        "camera" => Some(ctx.camera.clone()),
        _ => {
            // {seq:0N} zero-padded sequence.
            if let Some(width) = token.strip_prefix("seq:0") {
                if let Ok(w) = width.parse::<usize>() {
                    return Some(format!("{:0width$}", ctx.seq, width = w));
                }
            }
            None
        }
    }
}

/// Return a filename `"{stem}.{ext}"` unique within `taken`, appending `_1`,
/// `_2`, … to the stem on collision. The chosen name is inserted into `taken`.
pub fn resolve_collision(stem: &str, ext: &str, taken: &mut HashSet<String>) -> String {
    let base = format!("{stem}.{ext}");
    if taken.insert(base.clone()) {
        return base;
    }
    let mut n = 1usize;
    loop {
        let candidate = format!("{stem}_{n}.{ext}");
        if taken.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Convert an EXIF capture time "YYYY:MM:DD HH:MM:SS" to "YYYY-MM-DD".
/// Returns "" for `None` or anything that does not match the shape.
pub fn format_capture_date(capture_time: Option<&str>) -> String {
    let Some(s) = capture_time else {
        return String::new();
    };
    let date_part = s.split_whitespace().next().unwrap_or("");
    let comps: Vec<&str> = date_part.split(':').collect();
    if comps.len() == 3 && comps.iter().all(|c| !c.is_empty()) {
        format!("{}-{}-{}", comps[0], comps[1], comps[2])
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> FilenameCtx {
        FilenameCtx {
            name: "DSC_0001".into(),
            seq: 7,
            date: "2026-06-29".into(),
            camera: "Nikon Z f".into(),
        }
    }

    #[test]
    fn substitutes_known_tokens_and_literals() {
        assert_eq!(expand("{name}_{date}", &ctx()), "DSC_0001_2026-06-29");
        assert_eq!(expand("edit-{camera}", &ctx()), "edit-Nikon Z f");
        assert_eq!(expand("{name}", &ctx()), "DSC_0001");
    }

    #[test]
    fn seq_plain_and_zero_padded() {
        assert_eq!(expand("{seq}", &ctx()), "7");
        assert_eq!(expand("img_{seq:03}", &ctx()), "img_007");
        assert_eq!(expand("{seq:05}", &ctx()), "00007");
    }

    #[test]
    fn unknown_token_passes_through_verbatim() {
        assert_eq!(expand("{name}_{bogus}", &ctx()), "DSC_0001_{bogus}");
        assert_eq!(expand("plain text", &ctx()), "plain text");
    }

    #[test]
    fn collision_auto_suffix() {
        let mut taken = HashSet::new();
        assert_eq!(resolve_collision("photo", "jpg", &mut taken), "photo.jpg");
        assert_eq!(resolve_collision("photo", "jpg", &mut taken), "photo_1.jpg");
        assert_eq!(resolve_collision("photo", "jpg", &mut taken), "photo_2.jpg");
        assert_eq!(resolve_collision("other", "jpg", &mut taken), "other.jpg");
    }

    #[test]
    fn capture_date_formatting() {
        assert_eq!(
            format_capture_date(Some("2026:06:29 12:00:00")),
            "2026-06-29"
        );
        assert_eq!(format_capture_date(Some("garbage")), "");
        assert_eq!(format_capture_date(None), "");
    }
}
