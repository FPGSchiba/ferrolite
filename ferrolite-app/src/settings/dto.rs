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
}
