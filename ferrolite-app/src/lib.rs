// Thin library shim so the headless `bench_browse` binary can call
// `ingest::thumbnail_blocking` without duplicating decode logic.
// Only the modules the bench actually needs are declared here; the full
// UI module tree (app, chrome, canvas, etc.) lives in main.rs only.
pub mod events;
pub mod ingest;
pub mod library;
pub mod metadata;
pub mod state;
pub mod status_bar;
pub mod theme;
pub mod thumb_profile;
pub mod viewer;
pub mod widgets;
