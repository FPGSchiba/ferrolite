//! Hand-rolled XMP sidecar I/O. In Spec 1.5 the sidecar carries only
//! `xmp:Rating`; foreign nodes are preserved on write (merge-preserving).

use crate::error::CatalogError;
use ferrolite_image::Rating;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use quick_xml::Writer;
use std::path::{Path, PathBuf};

/// `<image>.xmp` next to the original (full filename + `.xmp`).
pub fn sidecar_path(image_path: &Path) -> PathBuf {
    let mut s = image_path.as_os_str().to_os_string();
    s.push(".xmp");
    PathBuf::from(s)
}

const RATING_LOCAL: &[u8] = b"xmp:Rating";

/// Read `xmp:Rating` (attribute OR element form). Lenient: any parse error or
/// missing file yields `None`.
pub fn read_rating(xmp_path: &Path) -> Option<Rating> {
    let text = std::fs::read_to_string(xmp_path).ok()?;
    let mut reader = Reader::from_str(&text);
    reader.config_mut().trim_text(true);
    let mut in_rating_elem = false;
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                // Attribute form on any element (typically rdf:Description).
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == RATING_LOCAL {
                        let v = String::from_utf8_lossy(&attr.value);
                        if let Ok(n) = v.trim().parse::<i64>() {
                            return Some(Rating::from_i64(n));
                        }
                    }
                }
                if e.name().as_ref() == RATING_LOCAL {
                    in_rating_elem = true;
                }
            }
            Ok(Event::Empty(e)) => {
                // Attribute form on self-closing element — do NOT set in_rating_elem.
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == RATING_LOCAL {
                        let v = String::from_utf8_lossy(&attr.value);
                        if let Ok(n) = v.trim().parse::<i64>() {
                            return Some(Rating::from_i64(n));
                        }
                    }
                }
            }
            Ok(Event::Text(t)) if in_rating_elem => {
                let v = t.unescape().unwrap_or_default();
                if let Ok(n) = v.trim().parse::<i64>() {
                    return Some(Rating::from_i64(n));
                }
                in_rating_elem = false;
            }
            Ok(Event::End(_)) => in_rating_elem = false,
            Err(_) => return None,
            _ => {}
        }
    }
    None
}

/// Build a minimal XMP sidecar string with only `xmp:Rating` set.
fn fresh_sidecar(rating: Rating) -> String {
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
         \x20<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
         \x20\x20<rdf:Description rdf:about=\"\" \
         xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"{}\"/>\n\
         \x20</rdf:RDF>\n\
         </x:xmpmeta>\n\
         <?xpacket end=\"w\"?>\n",
        rating.get()
    )
}

/// Build a copy of `src` (`rdf:Description` open-tag) with `xmp:Rating` set/replaced
/// as an attribute; all other attributes are preserved verbatim.
fn description_with_rating(src: &BytesStart<'_>, rating: Rating) -> BytesStart<'static> {
    let mut out = BytesStart::new(String::from_utf8_lossy(src.name().as_ref()).into_owned());
    for attr in src.attributes().flatten() {
        if attr.key.as_ref() != RATING_LOCAL {
            let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
            // Use unescape_value so the Writer re-escapes correctly, avoiding
            // double-escaping when the sidecar was previously written by the Writer.
            let val = attr
                .unescape_value()
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(&attr.value).into_owned());
            out.push_attribute((key.as_str(), val.as_str()));
        }
    }
    out.push_attribute(("xmp:Rating", rating.get().to_string().as_str()));
    out
}

/// Stream-rewrite an existing sidecar, returning the new bytes, or `None` on a
/// parse error (caller falls back to a fresh template + `.bak`).
///
/// Contract:
/// - Sets `xmp:Rating` as an ATTRIBUTE on the first `rdf:Description`.
/// - Drops any element-form `<xmp:Rating>…</xmp:Rating>` child.
/// - Stream-copies every other node verbatim (foreign nodes survive).
fn rewrite_with_rating(text: &str, rating: Rating) -> Option<Vec<u8>> {
    // Collect all events first so depth-tracking for element-form drop is clean.
    let mut reader = Reader::from_str(text);
    let mut events: Vec<Event<'static>> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(ev) => events.push(ev.into_owned()),
            Err(_) => return None,
        }
    }

    let mut writer = Writer::new(Vec::new());
    let mut done = false; // rating attribute already applied to first Description
    let mut skip_depth: Option<i32> = None; // Some(n) while skipping element-form xmp:Rating

    for ev in events {
        if let Some(depth) = skip_depth.as_mut() {
            // We are inside a skipped subtree.
            match &ev {
                Event::Start(_) => *depth += 1,
                Event::End(_) => {
                    if *depth == 0 {
                        // This End closes the element we started skipping.
                        skip_depth = None;
                        continue;
                    }
                    *depth -= 1;
                }
                Event::Empty(_) => {} // self-closing inside the subtree — no depth change
                _ => {}
            }
            continue; // drop this event
        }

        match ev {
            // Element-form <xmp:Rating> — start skipping until its matching End.
            Event::Start(ref e) if e.name().as_ref() == RATING_LOCAL => {
                skip_depth = Some(0);
                // do NOT emit
            }
            // Self-closing element-form <xmp:Rating/> — just drop it.
            Event::Empty(ref e) if e.name().as_ref() == RATING_LOCAL => {
                // drop
            }
            // First rdf:Description open-tag — inject/replace the rating attribute.
            Event::Start(ref e) if !done && e.name().as_ref() == b"rdf:Description" => {
                writer
                    .write_event(Event::Start(description_with_rating(e, rating)))
                    .ok()?;
                done = true;
            }
            // Self-closing rdf:Description — inject/replace the rating attribute.
            Event::Empty(ref e) if !done && e.name().as_ref() == b"rdf:Description" => {
                writer
                    .write_event(Event::Empty(description_with_rating(e, rating)))
                    .ok()?;
                done = true;
            }
            other => {
                writer.write_event(other).ok()?;
            }
        }
    }

    if !done {
        // No rdf:Description found — treat as structurally malformed.
        return None;
    }
    Some(writer.into_inner())
}

/// `<path>.xmp.bak` — backup location for malformed originals.
fn sidecar_bak(xmp_path: &Path) -> PathBuf {
    let mut s = xmp_path.as_os_str().to_os_string();
    s.push(".bak");
    PathBuf::from(s)
}

/// Write `xmp:Rating` into `xmp_path`, preserving any foreign nodes.
///
/// - Absent sidecar → write a minimal fresh template.
/// - Present sidecar → set `xmp:Rating` as attribute on first `rdf:Description`,
///   drop any element-form `<xmp:Rating>` child, stream-copy all other nodes.
/// - Parse error → rename original to `<path>.xmp.bak`, write fresh template.
pub fn write_rating(xmp_path: &Path, rating: Rating) -> Result<(), CatalogError> {
    match std::fs::read_to_string(xmp_path) {
        Ok(text) => match rewrite_with_rating(&text, rating) {
            Some(bytes) => std::fs::write(xmp_path, bytes)?,
            None => {
                // Malformed: back up, then write a fresh template.
                let bak = sidecar_bak(xmp_path);
                let _ = std::fs::rename(xmp_path, &bak);
                std::fs::write(xmp_path, fresh_sidecar(rating))?;
            }
        },
        Err(_) => std::fs::write(xmp_path, fresh_sidecar(rating))?,
    }
    Ok(())
}

const OPS_LOCAL: &[u8] = b"frl:ops";
const FRL_NS: &str = "http://ns.ferrolite.app/1.0/";

/// Read the `frl:ops` attribute payload (any element). Lenient: parse error or
/// missing file yields `None`.
pub fn read_ops(xmp_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(xmp_path).ok()?;
    let mut reader = Reader::from_str(&text);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == OPS_LOCAL {
                        return Some(attr.unescape_value().ok()?.into_owned());
                    }
                }
            }
            Err(_) => return None,
            _ => {}
        }
    }
    None
}

/// Build a copy of `src` (an `rdf:Description` open/empty tag) with `frl:ops` set/
/// replaced as an attribute, ensuring the `frl:` namespace decl is present; every
/// other attribute (incl. `xmp:Rating`, foreign `crs:`) is preserved verbatim.
fn description_with_ops(src: &BytesStart<'_>, ops: &str) -> BytesStart<'static> {
    let mut out = BytesStart::new(String::from_utf8_lossy(src.name().as_ref()).into_owned());
    let mut has_frl_ns = false;
    for attr in src.attributes().flatten() {
        if attr.key.as_ref() == OPS_LOCAL {
            continue; // replace
        }
        if attr.key.as_ref() == b"xmlns:frl" {
            has_frl_ns = true;
        }
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        // Use unescape_value so the Writer re-escapes correctly, avoiding
        // double-escaping when the sidecar was previously written by the Writer.
        let val = attr
            .unescape_value()
            .map(|v| v.into_owned())
            .unwrap_or_else(|_| String::from_utf8_lossy(&attr.value).into_owned());
        out.push_attribute((key.as_str(), val.as_str()));
    }
    if !has_frl_ns {
        out.push_attribute(("xmlns:frl", FRL_NS));
    }
    out.push_attribute(("frl:ops", ops));
    out
}

fn fresh_sidecar_ops(ops: &str) -> String {
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
         \x20<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
         \x20\x20<rdf:Description rdf:about=\"\" xmlns:frl=\"{FRL_NS}\" frl:ops=\"{ops}\"/>\n\
         \x20</rdf:RDF>\n\
         </x:xmpmeta>\n\
         <?xpacket end=\"w\"?>\n",
        // `ops` is JSON; quick-xml is not used for the fresh template, so escape
        // the five XML attribute-significant chars here.
        ops = xml_attr_escape(ops),
        FRL_NS = FRL_NS,
    )
}

/// Minimal XML attribute escaping for the hand-built fresh template.
fn xml_attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn rewrite_with_ops(text: &str, ops: &str) -> Option<Vec<u8>> {
    let mut reader = Reader::from_str(text);
    let mut events: Vec<Event<'static>> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(ev) => events.push(ev.into_owned()),
            Err(_) => return None,
        }
    }
    let mut writer = Writer::new(Vec::new());
    let mut done = false;
    for ev in events {
        match ev {
            Event::Start(ref e) if !done && e.name().as_ref() == b"rdf:Description" => {
                writer
                    .write_event(Event::Start(description_with_ops(e, ops)))
                    .ok()?;
                done = true;
            }
            Event::Empty(ref e) if !done && e.name().as_ref() == b"rdf:Description" => {
                writer
                    .write_event(Event::Empty(description_with_ops(e, ops)))
                    .ok()?;
                done = true;
            }
            other => {
                writer.write_event(other).ok()?;
            }
        }
    }
    if !done {
        return None;
    }
    Some(writer.into_inner())
}

/// Write the `frl:ops` payload into `xmp_path`, preserving `xmp:Rating` + foreign
/// nodes. Absent → fresh template; parse error → `.xmp.bak` + fresh template.
pub fn write_ops(xmp_path: &Path, ops_payload: &str) -> Result<(), CatalogError> {
    match std::fs::read_to_string(xmp_path) {
        Ok(text) => match rewrite_with_ops(&text, ops_payload) {
            Some(bytes) => std::fs::write(xmp_path, bytes)?,
            None => {
                let bak = sidecar_bak(xmp_path);
                let _ = std::fs::rename(xmp_path, &bak);
                std::fs::write(xmp_path, fresh_sidecar_ops(ops_payload))?;
            }
        },
        Err(_) => std::fs::write(xmp_path, fresh_sidecar_ops(ops_payload))?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_fresh_sidecar_when_absent() {
        let dir = std::env::temp_dir().join(format!("frl-xmp-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("d.xmp");
        let _ = std::fs::remove_file(&p);
        write_rating(&p, Rating::new(5)).unwrap();
        assert_eq!(read_rating(&p), Some(Rating::new(5)));
    }

    #[test]
    fn write_preserves_foreign_nodes_and_updates_rating() {
        let dir = std::env::temp_dir().join(format!("frl-xmp-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("e.xmp");
        std::fs::write(
            &p,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
                 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                   <rdf:Description rdf:about=""
                     xmlns:xmp="http://ns.adobe.com/xap/1.0/"
                     xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
                     xmp:Rating="1" crs:Exposure2012="+0.50">
                     <dc:subject xmlns:dc="http://purl.org/dc/elements/1.1/">
                       <rdf:Bag><rdf:li>portrait</rdf:li></rdf:Bag>
                     </dc:subject>
                   </rdf:Description>
                 </rdf:RDF>
               </x:xmpmeta>"#,
        )
        .unwrap();
        write_rating(&p, Rating::new(4)).unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("crs:Exposure2012"), "foreign attr preserved");
        assert!(out.contains("portrait"), "foreign dc:subject preserved");
        assert_eq!(read_rating(&p), Some(Rating::new(4)), "rating updated");
    }

    #[test]
    fn write_backs_up_malformed_then_writes_fresh() {
        let dir = std::env::temp_dir().join(format!("frl-xmp-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.xmp");
        std::fs::write(&p, "<broken <<").unwrap();
        write_rating(&p, Rating::new(3)).unwrap();
        assert!(
            dir.join("f.xmp.bak").exists(),
            "malformed original backed up"
        );
        assert_eq!(read_rating(&p), Some(Rating::new(3)));
    }

    #[test]
    fn sidecar_path_appends_xmp() {
        let p = sidecar_path(Path::new("/a/b/DSC_1.NEF"));
        assert_eq!(p, PathBuf::from("/a/b/DSC_1.NEF.xmp"));
    }

    #[test]
    fn reads_attribute_form_rating() {
        let dir = std::env::temp_dir().join(format!("frl-xmp-attr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.xmp");
        std::fs::write(
            &p,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
                 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                   <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/"
                     xmp:Rating="4"/>
                 </rdf:RDF>
               </x:xmpmeta>"#,
        )
        .unwrap();
        assert_eq!(read_rating(&p), Some(Rating::new(4)));
    }

    #[test]
    fn reads_element_form_rating() {
        let dir = std::env::temp_dir().join(format!("frl-xmp-elem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("b.xmp");
        std::fs::write(
            &p,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
                 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                   <rdf:Description rdf:about="">
                     <xmp:Rating xmlns:xmp="http://ns.adobe.com/xap/1.0/">2</xmp:Rating>
                   </rdf:Description>
                 </rdf:RDF>
               </x:xmpmeta>"#,
        )
        .unwrap();
        assert_eq!(read_rating(&p), Some(Rating::new(2)));
    }

    #[test]
    fn missing_or_malformed_is_none() {
        assert_eq!(read_rating(Path::new("/no/such.xmp")), None);
        let dir = std::env::temp_dir().join(format!("frl-xmp-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.xmp");
        std::fs::write(&p, "<not xml <<<").unwrap();
        assert_eq!(read_rating(&p), None);
    }

    #[allow(dead_code)]
    const FRL_NS_DECL: &str = "xmlns:frl=\"http://ns.ferrolite.app/1.0/\"";

    #[test]
    fn writes_fresh_ops_sidecar_when_absent() {
        let dir = std::env::temp_dir().join(format!("frl-ops-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("o.xmp");
        let _ = std::fs::remove_file(&p);
        write_ops(&p, r#"{"version":1,"ops":[]}"#).unwrap();
        assert_eq!(read_ops(&p).as_deref(), Some(r#"{"version":1,"ops":[]}"#));
    }

    #[test]
    fn write_ops_preserves_rating_and_foreign_nodes() {
        let dir = std::env::temp_dir().join(format!("frl-ops-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("o2.xmp");
        std::fs::write(
            &p,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
                 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                   <rdf:Description rdf:about=""
                     xmlns:xmp="http://ns.adobe.com/xap/1.0/"
                     xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
                     xmp:Rating="3" crs:Exposure2012="+0.50"/>
                 </rdf:RDF>
               </x:xmpmeta>"#,
        )
        .unwrap();
        write_ops(&p, r#"{"version":1,"ops":[{"Exposure":{"ev":0.5}}]}"#).unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("crs:Exposure2012"), "foreign attr preserved");
        assert_eq!(read_rating(&p), Some(Rating::new(3)), "rating preserved");
        assert_eq!(
            read_ops(&p).as_deref(),
            Some(r#"{"version":1,"ops":[{"Exposure":{"ev":0.5}}]}"#)
        );
    }

    #[test]
    fn ops_and_rating_writers_coexist_either_order() {
        let dir = std::env::temp_dir().join(format!("frl-ops-coexist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("o3.xmp");
        let _ = std::fs::remove_file(&p);
        write_ops(&p, r#"{"version":1,"ops":[]}"#).unwrap();
        write_rating(&p, Rating::new(5)).unwrap(); // must not clobber frl:ops
        assert_eq!(read_rating(&p), Some(Rating::new(5)));
        assert_eq!(read_ops(&p).as_deref(), Some(r#"{"version":1,"ops":[]}"#));
    }

    #[test]
    fn write_ops_backs_up_malformed_then_writes_fresh() {
        let dir = std::env::temp_dir().join(format!("frl-ops-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("o4.xmp");
        std::fs::write(&p, "<broken <<").unwrap();
        write_ops(&p, r#"{"version":1,"ops":[]}"#).unwrap();
        assert!(
            dir.join("o4.xmp.bak").exists(),
            "malformed original backed up"
        );
        assert_eq!(read_ops(&p).as_deref(), Some(r#"{"version":1,"ops":[]}"#));
    }

    #[test]
    fn read_ops_missing_or_malformed_is_none() {
        assert_eq!(read_ops(Path::new("/no/such.xmp")), None);
    }
}
