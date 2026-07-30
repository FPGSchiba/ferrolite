//! App-side serde DTOs for external types, so persistence adds no `serde`
//! dependency to engine-tier crates (§3.1) and keeps 4.1 self-contained.

use serde::{Deserialize, Serialize};

// ── Export ────────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PersistedFormat {
    Jpeg,
    Png,
    Tiff,
    WebP,
    Avif,
    JpegXl,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PersistedBitDepth {
    Eight,
    Sixteen,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PersistedResize {
    None,
    LongEdge(u32),
    Exact { w: u32, h: u32 },
    Percent(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum PersistedEffort {
    Fast,
    #[default]
    Balanced,
    Best,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PersistedExport {
    pub format: PersistedFormat,
    pub output_space: PersistedWorkingSpace,
    pub bit_depth: PersistedBitDepth,
    pub quality: u8,
    #[serde(default)]
    pub effort: PersistedEffort,
    pub resize: PersistedResize,
    pub copy_exif: bool,
    pub embed_icc: bool,
    pub strip_metadata: bool,
}

impl Default for PersistedExport {
    fn default() -> Self {
        Self::from_options(&ferrolite_export::ExportOptions::default())
    }
}

impl PersistedExport {
    pub fn from_options(o: &ferrolite_export::ExportOptions) -> Self {
        use ferrolite_export::{BitDepth, ExportFormat, ResizeSpec};
        Self {
            format: match o.format {
                ExportFormat::Jpeg => PersistedFormat::Jpeg,
                ExportFormat::Png => PersistedFormat::Png,
                ExportFormat::Tiff => PersistedFormat::Tiff,
                ExportFormat::WebP => PersistedFormat::WebP,
                ExportFormat::Avif => PersistedFormat::Avif,
                ExportFormat::JpegXl => PersistedFormat::JpegXl,
            },
            output_space: PersistedWorkingSpace::from_ws(o.output_space),
            bit_depth: match o.bit_depth {
                BitDepth::Eight => PersistedBitDepth::Eight,
                BitDepth::Sixteen => PersistedBitDepth::Sixteen,
            },
            quality: o.quality,
            effort: match o.effort {
                ferrolite_export::Effort::Fast => PersistedEffort::Fast,
                ferrolite_export::Effort::Balanced => PersistedEffort::Balanced,
                ferrolite_export::Effort::Best => PersistedEffort::Best,
            },
            resize: match o.resize {
                ResizeSpec::None => PersistedResize::None,
                ResizeSpec::LongEdge(p) => PersistedResize::LongEdge(p),
                ResizeSpec::Exact { w, h } => PersistedResize::Exact { w, h },
                ResizeSpec::Percent(p) => PersistedResize::Percent(p),
            },
            copy_exif: o.copy_exif,
            embed_icc: o.embed_icc,
            strip_metadata: o.strip_metadata,
        }
    }

    pub fn to_options(self) -> ferrolite_export::ExportOptions {
        use ferrolite_export::{BitDepth, ExportFormat, ExportOptions, ResizeSpec};
        ExportOptions {
            format: match self.format {
                PersistedFormat::Jpeg => ExportFormat::Jpeg,
                PersistedFormat::Png => ExportFormat::Png,
                PersistedFormat::Tiff => ExportFormat::Tiff,
                PersistedFormat::WebP => ExportFormat::WebP,
                PersistedFormat::Avif => ExportFormat::Avif,
                PersistedFormat::JpegXl => ExportFormat::JpegXl,
            },
            output_space: self.output_space.to_ws(),
            bit_depth: match self.bit_depth {
                PersistedBitDepth::Eight => BitDepth::Eight,
                PersistedBitDepth::Sixteen => BitDepth::Sixteen,
            },
            quality: self.quality,
            effort: match self.effort {
                PersistedEffort::Fast => ferrolite_export::Effort::Fast,
                PersistedEffort::Balanced => ferrolite_export::Effort::Balanced,
                PersistedEffort::Best => ferrolite_export::Effort::Best,
            },
            resize: match self.resize {
                PersistedResize::None => ResizeSpec::None,
                PersistedResize::LongEdge(p) => ResizeSpec::LongEdge(p),
                PersistedResize::Exact { w, h } => ResizeSpec::Exact { w, h },
                PersistedResize::Percent(p) => ResizeSpec::Percent(p),
            },
            copy_exif: self.copy_exif,
            embed_icc: self.embed_icc,
            strip_metadata: self.strip_metadata,
        }
    }
}

// ── Working space ───────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum PersistedWorkingSpace {
    Srgb,
    AdobeRgb,
    DisplayP3,
    #[default]
    Rec2020,
    ProPhoto,
}

impl PersistedWorkingSpace {
    pub fn from_ws(ws: ferrolite_color::WorkingSpace) -> Self {
        use ferrolite_color::WorkingSpace as W;
        match ws {
            W::Srgb => Self::Srgb,
            W::AdobeRgb => Self::AdobeRgb,
            W::DisplayP3 => Self::DisplayP3,
            W::Rec2020 => Self::Rec2020,
            W::ProPhoto => Self::ProPhoto,
        }
    }
    pub fn to_ws(self) -> ferrolite_color::WorkingSpace {
        use ferrolite_color::WorkingSpace as W;
        match self {
            Self::Srgb => W::Srgb,
            Self::AdobeRgb => W::AdobeRgb,
            Self::DisplayP3 => W::DisplayP3,
            Self::Rec2020 => W::Rec2020,
            Self::ProPhoto => W::ProPhoto,
        }
    }
}

// ── Filter (durable subset) ──────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum PersistedSortKey {
    #[default]
    CaptureTime,
    AddedAt,
    Filename,
    Rating,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum PersistedRatingCmp {
    #[default]
    AtLeast,
    Exactly,
    AtMost,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum PersistedTagMode {
    #[default]
    Any,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PersistedFlag {
    Pick,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PersistedFilter {
    pub sort_key: PersistedSortKey,
    pub sort_desc: bool,
    pub min_rating: u8,
    pub rating_cmp: PersistedRatingCmp,
    pub flags: Vec<PersistedFlag>,
    pub tag_ids: Vec<i64>,
    pub tag_mode: PersistedTagMode,
    pub include_subfolders: bool,
}

impl PersistedFilter {
    pub fn from_filter(f: &crate::library::filter::FilterState) -> Self {
        use crate::library::filter::RatingCmp;
        use ferrolite_catalog::{SortKey, TagMode};
        use ferrolite_image::Flag;
        Self {
            sort_key: match f.sort_key {
                SortKey::CaptureTime => PersistedSortKey::CaptureTime,
                SortKey::AddedAt => PersistedSortKey::AddedAt,
                SortKey::Filename => PersistedSortKey::Filename,
                SortKey::Rating => PersistedSortKey::Rating,
            },
            sort_desc: f.sort_desc,
            min_rating: f.min_rating,
            rating_cmp: match f.rating_cmp {
                RatingCmp::AtLeast => PersistedRatingCmp::AtLeast,
                RatingCmp::Exactly => PersistedRatingCmp::Exactly,
                RatingCmp::AtMost => PersistedRatingCmp::AtMost,
            },
            flags: f
                .flags
                .iter()
                .filter_map(|fl| match fl {
                    Flag::Pick => Some(PersistedFlag::Pick),
                    Flag::Reject => Some(PersistedFlag::Reject),
                    Flag::None => None,
                })
                .collect(),
            tag_ids: f.tag_ids.iter().map(|t| t.0).collect(),
            tag_mode: match f.tag_mode {
                TagMode::Any => PersistedTagMode::Any,
                TagMode::All => PersistedTagMode::All,
            },
            include_subfolders: true, // seeded by caller; see apply note
        }
    }

    /// Apply durable filter fields onto a base `FilterState` (leaving transient
    /// `search`/`camera`/`iso`/`date` at the base's values).
    pub fn apply_to(
        &self,
        mut base: crate::library::filter::FilterState,
    ) -> crate::library::filter::FilterState {
        use crate::library::filter::RatingCmp;
        use ferrolite_catalog::{SortKey, TagMode};
        use ferrolite_image::{Flag, TagId};
        base.sort_key = match self.sort_key {
            PersistedSortKey::CaptureTime => SortKey::CaptureTime,
            PersistedSortKey::AddedAt => SortKey::AddedAt,
            PersistedSortKey::Filename => SortKey::Filename,
            PersistedSortKey::Rating => SortKey::Rating,
        };
        base.sort_desc = self.sort_desc;
        base.min_rating = self.min_rating;
        base.rating_cmp = match self.rating_cmp {
            PersistedRatingCmp::AtLeast => RatingCmp::AtLeast,
            PersistedRatingCmp::Exactly => RatingCmp::Exactly,
            PersistedRatingCmp::AtMost => RatingCmp::AtMost,
        };
        base.flags = self
            .flags
            .iter()
            .map(|fl| match fl {
                PersistedFlag::Pick => Flag::Pick,
                PersistedFlag::Reject => Flag::Reject,
            })
            .collect();
        base.tag_ids = self.tag_ids.iter().map(|id| TagId(*id)).collect();
        base.tag_mode = match self.tag_mode {
            PersistedTagMode::Any => TagMode::Any,
            PersistedTagMode::All => TagMode::All,
        };
        base
    }
}

// ── Module ────────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum PersistedModule {
    #[default]
    Library,
    Develop,
    Export,
}

impl PersistedModule {
    pub fn from_module(m: crate::module::Module) -> Self {
        match m {
            crate::module::Module::Library => Self::Library,
            crate::module::Module::Develop => Self::Develop,
            crate::module::Module::Export => Self::Export,
        }
    }
    pub fn to_module(self) -> crate::module::Module {
        match self {
            Self::Library => crate::module::Module::Library,
            Self::Develop => crate::module::Module::Develop,
            Self::Export => crate::module::Module::Export,
        }
    }
}

// ── Display profile ──────────────────────────────────────────────────────────
use crate::monitor_profile::ProfileSource;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum PersistedDisplayProfile {
    #[default]
    Auto,
    Srgb,
    Custom(std::path::PathBuf),
}

/// Resolve the effective profile source. `Srgb` → None (analytic sRGB path);
/// `Custom` → the file; `Auto` → whatever detection found (may be None).
pub fn resolve(
    mode: &PersistedDisplayProfile,
    detected: Option<ProfileSource>,
) -> Option<ProfileSource> {
    match mode {
        PersistedDisplayProfile::Srgb => None,
        PersistedDisplayProfile::Custom(p) => Some(ProfileSource::Path(p.clone())),
        PersistedDisplayProfile::Auto => detected,
    }
}

// ── Settings ────────────────────────────────────────────────────────────────
pub fn default_true() -> bool {
    true
}

pub fn default_panel_width() -> f32 {
    300.0
}

pub fn default_filmstrip_height() -> f32 {
    96.0
}

/// Root persisted settings document. Every field defaults so older/partial
/// files load cleanly (forward/backward tolerant).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub keymap: super::keymap::Keymap,
    pub export: PersistedExport,
    pub filter: PersistedFilter,
    pub working_space: PersistedWorkingSpace,
    pub grid_size: f32,
    #[serde(default = "default_filmstrip_height")]
    pub filmstrip_height: f32,
    #[serde(default = "default_panel_width")]
    pub right_panel_width: f32,
    #[serde(default = "default_panel_width")]
    pub info_panel_width: f32,
    pub confirm_remove: bool,
    pub show_histogram: bool,
    pub show_info_overlay: bool,
    #[serde(default)]
    pub show_info_panel: bool,
    pub show_tool_palette: bool,
    pub restore_session: bool,
    pub last_module: PersistedModule,
    pub last_folder: Option<std::path::PathBuf>,
    pub display_profile: PersistedDisplayProfile,
    #[serde(default = "default_true")]
    pub basic_sliders_open: bool,
    #[serde(default = "default_true")]
    pub color_hsl_open: bool,
    #[serde(default = "default_true")]
    pub color_mix_open: bool,
    #[serde(default = "default_true")]
    pub sharpening_open: bool,
    #[serde(default = "default_true")]
    pub noise_reduction_open: bool,
    #[serde(default = "default_true")]
    pub dehaze_open: bool,
    #[serde(default = "default_true")]
    pub tone_curve_open: bool,
    #[serde(default = "default_true")]
    pub region_tones_open: bool,
    #[serde(default = "default_true")]
    pub color_grading_open: bool,
    #[serde(default = "default_true")]
    pub optics_open: bool,
    // per-scope disclosure state (spec §3 / V2 README): Mask scope remembers
    // its own open/closed sections independently of Adjust's flags above.
    #[serde(default = "default_true")]
    pub mask_basic_sliders_open: bool,
    #[serde(default = "default_true")]
    pub mask_tone_curve_open: bool,
    #[serde(default = "default_true")]
    pub mask_region_tones_open: bool,
    #[serde(default = "default_true")]
    pub mask_color_hsl_open: bool,
    #[serde(default = "default_true")]
    pub mask_color_mix_open: bool,
    #[serde(default = "default_true")]
    pub mask_color_grading_open: bool,
    #[serde(default = "default_true")]
    pub mask_sharpening_open: bool,
    #[serde(default = "default_true")]
    pub mask_noise_reduction_open: bool,
    #[serde(default = "default_true")]
    pub mask_dehaze_open: bool,
    // Crop tool's dedicated panel (design 2026-07-29 §C3 / V2 README:69) — the
    // panel that replaces the shared Light/Color/Effects tabs while Crop is
    // active. Only one scope exists (Crop is never mask-scoped), so there is
    // no `mask_crop_*` counterpart.
    #[serde(default = "default_true")]
    pub crop_transform_open: bool,
    #[serde(default = "default_true")]
    pub crop_geometry_open: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keymap: super::keymap::Keymap::defaults(),
            export: PersistedExport::default(),
            filter: PersistedFilter::default(),
            working_space: PersistedWorkingSpace::default(),
            grid_size: 46.0,
            filmstrip_height: default_filmstrip_height(),
            right_panel_width: default_panel_width(),
            info_panel_width: default_panel_width(),
            confirm_remove: true,
            show_histogram: true,
            show_info_overlay: false,
            show_info_panel: false,
            show_tool_palette: true,
            restore_session: false,
            last_module: PersistedModule::default(),
            last_folder: None,
            display_profile: PersistedDisplayProfile::default(),
            basic_sliders_open: true,
            color_hsl_open: true,
            color_mix_open: true,
            sharpening_open: true,
            noise_reduction_open: true,
            dehaze_open: true,
            tone_curve_open: true,
            region_tones_open: true,
            color_grading_open: true,
            optics_open: true,
            mask_basic_sliders_open: true,
            mask_tone_curve_open: true,
            mask_region_tones_open: true,
            mask_color_hsl_open: true,
            mask_color_mix_open: true,
            mask_color_grading_open: true,
            mask_sharpening_open: true,
            mask_noise_reduction_open: true,
            mask_dehaze_open: true,
            crop_transform_open: true,
            crop_geometry_open: true,
        }
    }
}

/// Number of section-disclosure (`*_open`) flags in `Settings` — see
/// `disclosure_snapshot`. Kept as a named constant so the snapshot array size
/// and the coverage test share one source of truth.
pub const DISCLOSURE_FLAG_COUNT: usize = 21;

/// Snapshot of EVERY section-disclosure flag on `Settings` (both the Adjust
/// scope and its per-scope Mask counterpart — see the `mask_*_open` fields'
/// doc comment). Callers diff two snapshots taken across a frame to detect
/// whether ANY disclosure toggle changed, so the settings-dirty flag can be
/// set once regardless of which specific section was (dis)closed — see
/// `FerroliteApp`'s Develop tool-panel frame, which used to hand-diff only 3
/// of these fields and silently missed the rest.
///
/// Order is arbitrary — only whole-array equality matters. If you add a new
/// section-disclosure field to `Settings`, add it here too: the
/// `disclosure_snapshot_covers_every_open_field` test below fails otherwise
/// (it counts the matching field declarations in this file's own source),
/// which is exactly the tripwire this helper exists to provide.
pub fn disclosure_snapshot(s: &Settings) -> [bool; DISCLOSURE_FLAG_COUNT] {
    [
        s.basic_sliders_open,
        s.color_hsl_open,
        s.color_mix_open,
        s.sharpening_open,
        s.noise_reduction_open,
        s.dehaze_open,
        s.tone_curve_open,
        s.region_tones_open,
        s.color_grading_open,
        s.optics_open,
        s.mask_basic_sliders_open,
        s.mask_tone_curve_open,
        s.mask_region_tones_open,
        s.mask_color_hsl_open,
        s.mask_color_mix_open,
        s.mask_color_grading_open,
        s.mask_sharpening_open,
        s.mask_noise_reduction_open,
        s.mask_dehaze_open,
        s.crop_transform_open,
        s.crop_geometry_open,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_roundtrip_all_variants() {
        use ferrolite_export::{BitDepth, ExportFormat, ExportOptions, ResizeSpec};
        let cases = [
            ExportOptions::default(),
            ExportOptions {
                format: ExportFormat::Tiff,
                bit_depth: BitDepth::Sixteen,
                resize: ResizeSpec::LongEdge(2048),
                ..ExportOptions::default()
            },
            ExportOptions {
                format: ExportFormat::WebP,
                resize: ResizeSpec::Exact { w: 800, h: 600 },
                copy_exif: false,
                embed_icc: false,
                strip_metadata: true,
                ..ExportOptions::default()
            },
            ExportOptions {
                resize: ResizeSpec::Percent(0.5),
                quality: 72,
                ..ExportOptions::default()
            },
        ];
        for opts in cases {
            let round = PersistedExport::from_options(&opts).to_options();
            assert_eq!(round, opts);
        }
    }

    #[test]
    fn working_space_roundtrip_all() {
        use ferrolite_color::WorkingSpace;
        for ws in WorkingSpace::ALL {
            assert_eq!(PersistedWorkingSpace::from_ws(ws).to_ws(), ws);
        }
    }

    #[test]
    fn filter_roundtrip_preserves_durable_fields() {
        use crate::library::filter::{FilterState, RatingCmp};
        use ferrolite_catalog::{SortKey, TagMode};
        use ferrolite_image::Flag;
        let f = FilterState {
            sort_key: SortKey::CaptureTime,
            sort_desc: true,
            min_rating: 3,
            rating_cmp: RatingCmp::AtMost,
            flags: vec![Flag::Pick],
            tag_mode: TagMode::All,
            ..FilterState::default()
        };
        let round = PersistedFilter::from_filter(&f).apply_to(FilterState::default());
        assert_eq!(round.sort_key, f.sort_key);
        assert_eq!(round.sort_desc, f.sort_desc);
        assert_eq!(round.min_rating, f.min_rating);
        assert_eq!(round.rating_cmp, f.rating_cmp);
        assert_eq!(round.flags, f.flags);
        assert_eq!(round.tag_mode, f.tag_mode);
    }

    #[test]
    fn module_roundtrip() {
        use crate::module::Module;
        for m in [Module::Library, Module::Develop, Module::Export] {
            assert_eq!(PersistedModule::from_module(m).to_module(), m);
        }
    }

    #[test]
    fn export_roundtrip_preserves_effort_and_new_formats() {
        use ferrolite_export::{Effort, ExportFormat, ExportOptions};
        for opts in [
            ExportOptions {
                format: ExportFormat::Avif,
                effort: Effort::Best,
                quality: 80,
                ..ExportOptions::default()
            },
            ExportOptions {
                format: ExportFormat::JpegXl,
                bit_depth: ferrolite_export::BitDepth::Sixteen,
                ..ExportOptions::default()
            },
            ExportOptions {
                format: ExportFormat::Avif,
                effort: Effort::Fast,
                ..ExportOptions::default()
            },
        ] {
            let round = PersistedExport::from_options(&opts).to_options();
            assert_eq!(round, opts);
        }
    }

    #[test]
    fn legacy_export_json_without_effort_defaults_to_balanced() {
        // A settings blob serialized before `effort` existed must still deserialize.
        let legacy = r#"{
            "format":"Jpeg","output_space":"Rec2020","bit_depth":"Eight",
            "quality":90,"resize":"None","copy_exif":true,"embed_icc":true,"strip_metadata":false
        }"#;
        let parsed: PersistedExport = serde_json::from_str(legacy).expect("deserialize legacy");
        assert_eq!(parsed.effort, PersistedEffort::Balanced);
    }

    #[test]
    fn resolve_srgb_is_none() {
        assert!(resolve(
            &PersistedDisplayProfile::Srgb,
            Some(ProfileSource::Bytes(vec![1]))
        )
        .is_none());
    }

    #[test]
    fn resolve_custom_uses_path_even_when_detected_present() {
        let p = std::path::PathBuf::from("x.icc");
        let r = resolve(
            &PersistedDisplayProfile::Custom(p.clone()),
            Some(ProfileSource::Bytes(vec![9])),
        );
        assert!(matches!(r, Some(ProfileSource::Path(pp)) if pp == p));
    }

    #[test]
    fn resolve_auto_passes_detected_through() {
        assert!(resolve(&PersistedDisplayProfile::Auto, None).is_none());
        assert!(matches!(
            resolve(
                &PersistedDisplayProfile::Auto,
                Some(ProfileSource::Bytes(vec![7]))
            ),
            Some(ProfileSource::Bytes(_))
        ));
    }

    #[test]
    fn display_profile_roundtrips_through_json() {
        for m in [
            PersistedDisplayProfile::Auto,
            PersistedDisplayProfile::Srgb,
            PersistedDisplayProfile::Custom("/tmp/p.icc".into()),
        ] {
            let js = serde_json::to_string(&m).unwrap();
            assert_eq!(
                serde_json::from_str::<PersistedDisplayProfile>(&js).unwrap(),
                m
            );
        }
    }

    #[test]
    fn settings_layout_fields_defaults_and_json_roundtrip() {
        let default_settings = Settings::default();
        assert!(!default_settings.show_info_panel);
        assert_eq!(default_settings.filmstrip_height, 96.0);
        assert_eq!(default_settings.right_panel_width, 300.0);
        assert_eq!(default_settings.info_panel_width, 300.0);
        assert!(default_settings.basic_sliders_open);
        assert!(default_settings.color_hsl_open);
        assert!(default_settings.color_mix_open);
        assert!(default_settings.sharpening_open);
        assert!(default_settings.noise_reduction_open);
        assert!(default_settings.dehaze_open);
        assert!(default_settings.tone_curve_open);
        assert!(default_settings.region_tones_open);
        assert!(default_settings.color_grading_open);
        assert!(default_settings.optics_open);
        assert!(default_settings.mask_basic_sliders_open);
        assert!(default_settings.mask_tone_curve_open);
        assert!(default_settings.mask_region_tones_open);
        assert!(default_settings.mask_color_hsl_open);
        assert!(default_settings.mask_color_mix_open);
        assert!(default_settings.mask_color_grading_open);
        assert!(default_settings.mask_sharpening_open);
        assert!(default_settings.mask_noise_reduction_open);
        assert!(default_settings.mask_dehaze_open);
        assert!(default_settings.crop_transform_open);
        assert!(default_settings.crop_geometry_open);

        let empty_json = "{}";
        let parsed: Settings = serde_json::from_str(empty_json).expect("deserialize empty json");
        assert_eq!(parsed, default_settings);

        let custom = Settings {
            show_info_panel: true,
            filmstrip_height: 140.0,
            right_panel_width: 350.0,
            info_panel_width: 250.0,
            basic_sliders_open: false,
            color_hsl_open: false,
            color_mix_open: false,
            sharpening_open: false,
            noise_reduction_open: false,
            dehaze_open: false,
            tone_curve_open: false,
            region_tones_open: false,
            color_grading_open: false,
            optics_open: false,
            mask_basic_sliders_open: false,
            mask_tone_curve_open: false,
            mask_region_tones_open: false,
            mask_color_hsl_open: false,
            mask_color_mix_open: false,
            mask_color_grading_open: false,
            mask_sharpening_open: false,
            mask_noise_reduction_open: false,
            mask_dehaze_open: false,
            crop_transform_open: false,
            crop_geometry_open: false,
            ..Settings::default()
        };

        let serialized = serde_json::to_string(&custom).expect("serialize custom settings");
        let deserialized: Settings =
            serde_json::from_str(&serialized).expect("deserialize custom settings");
        assert_eq!(deserialized, custom);
    }

    /// Tripwire for `disclosure_snapshot`: counts field declarations of the
    /// shape `<name>_open: bool,` in this file's own source (a new `*_open`
    /// field on `Settings` adds exactly one more such declaration — note the
    /// trailing `,` + newline, so this can't self-match the prose describing
    /// it) and asserts it matches `DISCLOSURE_FLAG_COUNT` / the snapshot
    /// array's length. A future section's disclosure flag can no longer
    /// silently fall out of the settings-dirty tracking the way
    /// `tone_curve_open` / `color_grading_open` / `optics_open`-only
    /// hand-diffing once did.
    #[test]
    fn disclosure_snapshot_covers_every_open_field() {
        let field_declarations = include_str!("dto.rs").matches("_open: bool,\n").count();
        assert_eq!(
            field_declarations, DISCLOSURE_FLAG_COUNT,
            "a `*_open` field was added to/removed from Settings without updating \
             DISCLOSURE_FLAG_COUNT and disclosure_snapshot"
        );
        assert_eq!(
            disclosure_snapshot(&Settings::default()).len(),
            DISCLOSURE_FLAG_COUNT
        );
    }
}
