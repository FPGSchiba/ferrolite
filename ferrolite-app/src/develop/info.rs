//! Pure, egui-free formatting of image EXIF + live viewer zoom into display
//! strings. Shared by the info overlay and the Info tab.

use ferrolite_decode::Metadata;

/// On-screen magnification relative to the fit transform, as a percent.
/// At the fit zoom this returns 100.
pub fn zoom_percent(view_zoom: f32, fit_zoom: f32) -> u32 {
    if fit_zoom <= 0.0 {
        return 100;
    }
    (view_zoom / fit_zoom * 100.0).round() as u32
}

fn fmt_shutter(secs: f32) -> String {
    if secs <= 0.0 {
        String::new()
    } else if secs >= 1.0 {
        format!("{secs:.0}\"")
    } else {
        format!("1/{}", (1.0 / secs).round() as u32)
    }
}

pub struct ImageFacts {
    pub camera: String,
    pub lens: String,
    pub focal: String,
    pub aperture: String,
    pub shutter: String,
    pub iso: String,
    pub capture_time: String,
    pub dimensions: String,
    pub zoom: String,
}

impl ImageFacts {
    pub fn build(meta: &Metadata, view_zoom: f32, fit_zoom: f32, dims: (u32, u32)) -> Self {
        let focal = match (meta.focal_length, meta.focal_length_35mm) {
            (Some(f), Some(eq)) => format!("{:.0}mm ({eq}mm eq.)", f),
            (Some(f), None) => format!("{:.0}mm", f),
            (None, _) => String::new(),
        };
        ImageFacts {
            camera: format!("{} {}", meta.make, meta.model).trim().to_string(),
            lens: meta.lens.clone().unwrap_or_default(),
            focal,
            aperture: meta
                .aperture
                .map(|a| format!("f/{a:.1}"))
                .unwrap_or_default(),
            shutter: meta.shutter.map(fmt_shutter).unwrap_or_default(),
            iso: meta.iso.map(|v| format!("ISO {v}")).unwrap_or_default(),
            capture_time: meta.capture_time.clone().unwrap_or_default(),
            dimensions: format!("{} × {}", dims.0, dims.1),
            zoom: format!("{}%", zoom_percent(view_zoom, fit_zoom)),
        }
    }
}

#[cfg(test)]
mod tests {
    use ferrolite_decode::Metadata;
    use ferrolite_image::Orientation;

    fn meta() -> Metadata {
        Metadata {
            make: "FUJIFILM".into(),
            model: "X-T5".into(),
            width: 6000,
            height: 4000,
            orientation: Orientation::Normal,
            iso: Some(400),
            aperture: Some(2.8),
            shutter: Some(1.0 / 250.0),
            focal_length: Some(35.0),
            focal_length_35mm: Some(53),
            capture_time: Some("2026:01:02 10:11:12".into()),
            lens: Some("XF35mmF1.4 R".into()),
        }
    }

    #[test]
    fn zoom_percent_is_relative_to_fit() {
        assert_eq!(super::zoom_percent(0.2, 0.2), 100);
        assert_eq!(super::zoom_percent(0.4, 0.2), 200);
    }

    #[test]
    fn facts_format_focal_with_equiv() {
        let f = super::ImageFacts::build(&meta(), 0.2, 0.2, (6000, 4000));
        assert_eq!(f.focal, "35mm (53mm eq.)");
        assert_eq!(f.aperture, "f/2.8");
        assert_eq!(f.iso, "ISO 400");
        assert_eq!(f.dimensions, "6000 × 4000");
        assert_eq!(f.zoom, "100%");
    }

    #[test]
    fn facts_omit_equiv_when_absent() {
        let mut m = meta();
        m.focal_length_35mm = None;
        let f = super::ImageFacts::build(&m, 0.2, 0.2, (6000, 4000));
        assert_eq!(f.focal, "35mm");
    }
}
