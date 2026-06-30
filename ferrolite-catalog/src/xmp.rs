//! Hand-rolled XMP sidecar I/O. In Spec 1.5 the sidecar carries only
//! `xmp:Rating`; foreign nodes are preserved on write (merge-preserving).

use ferrolite_image::Rating;
use quick_xml::events::Event;
use quick_xml::Reader;
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
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
