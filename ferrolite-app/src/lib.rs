// Thin library shim so the headless `bench_browse` binary can call
// `ingest::thumbnail_blocking` without duplicating decode logic.
// Only the modules the bench actually needs are declared here; the full
// UI module tree (app, chrome, canvas, etc.) lives in main.rs only.
pub mod app;
pub mod camera_matrix;
pub mod canvas;
pub mod chrome;
pub mod develop;
pub mod diag;
pub mod diag_mem;
pub mod events;
pub mod export;
pub mod export_module;
pub mod help;
pub mod icons;
pub mod ingest;
pub mod library;
pub mod mem_probe;
pub mod metadata;
pub mod module;
pub mod monitor_profile;
pub mod notifications;
pub mod read_gate;
pub mod settings;
pub mod state;
pub mod status_bar;
pub mod theme;
pub mod viewer;
pub mod widgets;
