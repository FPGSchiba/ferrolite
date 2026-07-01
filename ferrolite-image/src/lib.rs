//! Core pixel/orientation vocabulary shared across ferrolite crates.

mod file_kind;
mod linear;
mod meta;
mod orientation;
mod pixel;
mod tile;

pub use file_kind::FileKind;
pub use linear::LinearRgbaF32;
pub use meta::{Color, Flag, Rating, TagId};
pub use orientation::Orientation;
pub use pixel::{ImageBuffer, ImageBufferError, PixelFormat};
pub use tile::{
    haloed_tile_extent, haloed_tile_origin, level_size, pyramid_level_count, tile_pixel_origin,
    tiles_per_level, TileCoord, TILE_SIZE,
};
