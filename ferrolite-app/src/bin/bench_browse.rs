//! Headless M1 benchmark harness for ferrolite.
//!
//! Opens a fresh temp catalog, scans a folder with `scan_raw_files`, upserts
//! all rows, then generates thumbnails using the same `thumbnail_blocking`
//! helper that the interactive ingest path calls — so this exercises the real
//! decode + resize + persist pipeline without any egui dependency.
//!
//! Outputs:
//!   M1a  rows-indexed-complete (wall-clock ms)
//!   M1b  first-N-thumbnails-committed (wall-clock ms)
//!   throughput  thumbnails/sec over the full run
//!
//! Usage:
//!   cargo run -p ferrolite-app --bin bench_browse -- <folder-path> [N]
//!
//!   folder-path  directory containing RAW files
//!   N            number of thumbnails to time for M1b (default: 100)

use ferrolite_app::ingest::thumbnail_blocking;
use ferrolite_catalog::{
    scan_raw_files, Catalog, DecodeStatus, FileKind, NewImage, ThumbnailStore,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn usage() -> ! {
    eprintln!("Usage: bench_browse <folder-path> [N]");
    eprintln!("  folder-path  directory containing RAW files to benchmark");
    eprintln!("  N            thumbnails to count for M1b (default: 100)");
    std::process::exit(1);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let folder: PathBuf = match args.next() {
        Some(p) => PathBuf::from(p),
        None => usage(),
    };
    let n: usize = match args.next() {
        Some(s) => s.parse().unwrap_or_else(|_| {
            eprintln!("error: N must be a positive integer, got {s:?}");
            usage();
        }),
        None => 100,
    };

    if !folder.is_dir() {
        eprintln!("error: {:?} is not a directory", folder);
        usage();
    }

    // Open a fresh temp catalog (deleted on exit).
    let db_path = std::env::temp_dir().join(format!("ferrolite-bench-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let catalog = Catalog::open(&db_path).expect("failed to open temp catalog");
    let writer: Arc<Mutex<Catalog>> = Arc::new(Mutex::new(catalog));

    println!("ferrolite bench_browse");
    println!("  folder : {}", folder.display());
    println!("  N      : {n}");
    println!("  db     : {}", db_path.display());
    println!();

    // ------------------------------------------------------------------
    // M1a: scan + upsert all rows (no thumbnail decode)
    // ------------------------------------------------------------------
    let t0 = Instant::now();

    let folder_id = writer
        .lock()
        .expect("writer lock")
        .upsert_folder(&folder, None)
        .expect("upsert_folder failed");

    let files = scan_raw_files(&folder);
    println!("  {} RAW files found", files.len());

    let mut image_ids: Vec<(i64, PathBuf)> = Vec::with_capacity(files.len());
    for f in &files {
        let kind = f.kind;
        let new_image = match ferrolite_decode::read_metadata(&f.path, kind) {
            Ok(meta) => NewImage::from_metadata(
                folder_id,
                f.filename.clone(),
                f.mtime,
                f.size,
                &meta,
                kind,
                ferrolite_catalog::Rating::default(),
                0,
            ),
            Err(_) => NewImage::failed(folder_id, f.filename.clone(), f.mtime, f.size, kind, 0),
        };
        match writer.lock().expect("writer lock").upsert_image(&new_image) {
            Ok(id) => {
                if new_image.decode_status != DecodeStatus::Failed {
                    image_ids.push((id, f.path.clone()));
                }
            }
            Err(e) => eprintln!("  warn: upsert_image failed: {e}"),
        }
    }

    let m1a_ms = t0.elapsed().as_millis();
    println!(
        "M1a  rows-indexed-complete : {m1a_ms} ms  ({} decodable)",
        image_ids.len()
    );

    // ------------------------------------------------------------------
    // M1b: first-N thumbnails committed
    // ------------------------------------------------------------------
    let n_actual = n.min(image_ids.len());
    if n_actual == 0 {
        println!("M1b  no decodable images found — skipping thumbnail benchmark");
        cleanup(&db_path);
        return;
    }

    let t1 = Instant::now();
    let mut done = 0usize;
    let mut errors = 0usize;

    for (image_id, path) in &image_ids {
        match thumbnail_blocking(&writer, *image_id, path, FileKind::Raw) {
            Ok(_) => done += 1,
            Err(e) => {
                errors += 1;
                eprintln!("  warn: thumbnail failed for #{image_id}: {e}");
            }
        }
        if done >= n_actual {
            break;
        }
    }

    let m1b_ms = t1.elapsed().as_millis();
    let total_thumb_ms = t0.elapsed().as_millis() - m1a_ms + m1b_ms;

    // Continue decoding remaining images for throughput number.
    for (image_id, path) in image_ids.iter().skip(n_actual) {
        match thumbnail_blocking(&writer, *image_id, path, FileKind::Raw) {
            Ok(_) => done += 1,
            Err(_) => errors += 1,
        }
    }

    let total_ms = t0.elapsed().as_millis();
    let throughput = if total_ms > 0 {
        done as f64 / (total_ms as f64 / 1000.0)
    } else {
        0.0
    };

    // Verify a thumbnail is actually in the DB for the first image.
    let first_verified = if let Some((id, _)) = image_ids.first() {
        writer
            .lock()
            .expect("writer lock")
            .get_thumbnail(*id)
            .map(|t| t.is_some())
            .unwrap_or(false)
    } else {
        false
    };

    println!("M1b  first-{n_actual}-thumbnails-committed : {m1b_ms} ms");
    println!();
    println!("  thumbnails decoded  : {done}");
    println!("  decode errors       : {errors}");
    println!("  throughput          : {throughput:.1} thumbnails/sec");
    println!(
        "  db round-trip check : {}",
        if first_verified { "OK" } else { "FAIL" }
    );
    println!("  total wall time     : {total_ms} ms");
    println!(
        "  (M1a elapsed during thumbnail total: {}ms subtracted)",
        total_thumb_ms - m1b_ms
    );

    cleanup(&db_path);
}

fn cleanup(db_path: &std::path::Path) {
    let _ = std::fs::remove_file(db_path);
    // WAL side-car files
    let wal = db_path.with_extension("db-wal");
    let shm = db_path.with_extension("db-shm");
    let _ = std::fs::remove_file(wal);
    let _ = std::fs::remove_file(shm);
}
