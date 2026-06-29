use ferrolite_catalog::Catalog;

#[test]
fn fresh_db_is_migrated_to_current_version() {
    let cat = Catalog::open_in_memory().expect("open");
    assert_eq!(
        cat.schema_version().expect("version"),
        ferrolite_catalog::SCHEMA_VERSION
    );
}

#[test]
fn migrate_is_idempotent_on_reopen() {
    let dir = tempdir();
    let path = dir.join("catalog.db");
    {
        let _ = Catalog::open(&path).expect("first open");
    }
    // Reopening an already-migrated DB must not error or downgrade.
    let cat = Catalog::open(&path).expect("second open");
    assert_eq!(
        cat.schema_version().expect("version"),
        ferrolite_catalog::SCHEMA_VERSION
    );
}

/// Minimal temp dir without an extra dependency: unique path under the OS temp
/// dir using the test thread name + a process-unique counter.
fn tempdir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("ferrolite-cat-{pid}-{n}"));
    std::fs::create_dir_all(&dir).expect("mkdir temp");
    dir
}
