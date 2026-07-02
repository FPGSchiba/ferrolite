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
}

/// Expand `template` against `ctx`. Recognised tokens: `{name}`, `{seq}`,
/// `{seq:0N}` (zero-padded to width N), `{date}`. Any other
/// `{...}` run is emitted verbatim (braces included). Literal text passes through.
pub fn expand(template: &str, ctx: &FilenameCtx) -> String {
    let mut out = String::with_capacity(template.len() + 8);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        match after.find('}') {
            Some(close) => {
                let token = &after[1..close];
                match resolve_token(token, ctx) {
                    // Recognised token → substituted value.
                    Some(rep) => out.push_str(&rep),
                    // Unknown token → emit verbatim, braces included.
                    None => out.push_str(&after[..=close]),
                }
                rest = &after[close + 1..];
            }
            // Unterminated '{' → emit the remainder literally.
            None => {
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn resolve_token(token: &str, ctx: &FilenameCtx) -> Option<String> {
    match token {
        "name" => Some(ctx.name.clone()),
        "seq" => Some(ctx.seq.to_string()),
        "date" => Some(ctx.date.clone()),
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

/// Make a filename component safe across OS filesystems: replace characters
/// illegal on Windows (`< > : " / \ | ? *`) and ASCII control chars with `_`,
/// then trim trailing dots/spaces (illegal on Windows). Empty result → "export".
pub fn sanitize_component(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("export");
    }
    out
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
        }
    }

    #[test]
    fn substitutes_known_tokens_and_literals() {
        assert_eq!(expand("{name}_{date}", &ctx()), "DSC_0001_2026-06-29");
        assert_eq!(expand("{name}", &ctx()), "DSC_0001");
    }

    #[test]
    fn camera_token_is_now_unknown_and_passes_through_verbatim() {
        // {camera} was removed (no source data on ImageRecord); it now falls
        // through the unknown-token verbatim path.
        assert_eq!(expand("edit-{camera}", &ctx()), "edit-{camera}");
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

    #[test]
    fn sanitize_replaces_illegal_windows_chars() {
        assert_eq!(sanitize_component("a/b:c*d"), "a_b_c_d");
        assert_eq!(sanitize_component("<>\"|?\\"), "______");
    }

    #[test]
    fn sanitize_replaces_control_chars() {
        assert_eq!(sanitize_component("a\u{0}b\tc"), "a_b_c");
    }

    #[test]
    fn sanitize_trims_trailing_dots_and_spaces() {
        assert_eq!(sanitize_component("name. "), "name");
        assert_eq!(sanitize_component("name..."), "name");
        assert_eq!(sanitize_component("name   "), "name");
    }

    #[test]
    fn sanitize_all_illegal_or_empty_becomes_export() {
        assert_eq!(sanitize_component(""), "export");
        assert_eq!(sanitize_component("..."), "export");
        // '/' is replaced with '_' (not trimmed), so this is not empty.
        assert_eq!(sanitize_component("///"), "___");
    }

    #[test]
    fn sanitize_normal_name_passes_through_unchanged() {
        assert_eq!(sanitize_component("DSC_0001"), "DSC_0001");
    }

    #[test]
    fn sanitize_unicode_name_passes_through_unchanged() {
        assert_eq!(sanitize_component("café"), "café");
    }

    #[test]
    fn non_ascii_literal_text_is_preserved() {
        // café / ü / – must survive verbatim around a token.
        assert_eq!(expand("café_{name}_ü–", &ctx()), "café_DSC_0001_ü–");
    }

    #[test]
    fn unterminated_brace_is_literal() {
        assert_eq!(expand("a{name}_{oops", &ctx()), "aDSC_0001_{oops");
    }
}
