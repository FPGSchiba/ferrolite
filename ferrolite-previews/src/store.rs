//! `PreviewStore` — a content-addressed on-disk store for cached preview
//! JPEGs. Each entry is two files: `<digest>.jpg` (the JPEG payload) and
//! `<digest>.key` (the full [`PreviewKey`] as JSON, for collision-safety —
//! a digest match alone is never trusted as a cache hit; the stored key
//! must compare byte-exact to the lookup key).
//!
//! Pure disk I/O: no GPU/UI/threads. Writes are atomic (temp file + rename)
//! so a crash mid-`put` never leaves a partially-written entry visible to
//! readers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::key::PreviewKey;

/// A directory-backed, content-addressed cache of preview JPEGs.
pub struct PreviewStore {
    dir: PathBuf,
}

impl PreviewStore {
    /// Opens (creating if absent) a `PreviewStore` rooted at `dir`.
    pub fn new(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// `true` if `key` has a cached payload on disk whose stored `.key`
    /// compares byte-exact to `key` (a digest match alone is not enough).
    pub fn contains(&self, key: &PreviewKey) -> bool {
        let digest = key.digest();
        self.jpg_path(&digest).is_file() && self.key_matches(&digest, key)
    }

    /// Returns the cached JPEG payload on an exact-key hit, touching the
    /// entry's last-access time (the `.key` file's mtime) in the process.
    /// Any I/O error, missing entry, or key mismatch is a miss (`None`) —
    /// this never panics.
    pub fn get(&self, key: &PreviewKey) -> Option<Vec<u8>> {
        let digest = key.digest();
        if !self.key_matches(&digest, key) {
            return None;
        }
        let bytes = fs::read(self.jpg_path(&digest)).ok()?;
        self.touch_key(&digest);
        Some(bytes)
    }

    /// Atomically writes `<digest>.jpg` + `<digest>.key` (temp file +
    /// rename for each). On success, exactly the two final files exist and
    /// no `*.tmp` is left behind.
    pub fn put(&self, key: &PreviewKey, jpeg: &[u8]) -> io::Result<()> {
        let digest = key.digest();
        let key_json = serde_json::to_vec(key).map_err(io::Error::other)?;

        // Write the payload first, then the `.key`: the `.key` file is the
        // commit marker every lookup consults first (`key_matches`), so a
        // crash between the two writes leaves an orphaned `.jpg` that no
        // lookup will ever serve, rather than a `.key` pointing at a
        // missing payload.
        atomic_write(&self.jpg_path(&digest), jpeg)?;
        atomic_write(&self.key_path(&digest), &key_json)?;
        Ok(())
    }

    /// Sum of the sizes of all cached JPEG payloads (`*.jpg`) in the store.
    pub fn total_bytes(&self) -> u64 {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|entry| has_extension(&entry.path(), "jpg"))
            .filter_map(|entry| entry.metadata().ok())
            .map(|meta| meta.len())
            .sum()
    }

    /// Removes every cached entry (and any stray temp file) from the
    /// store, leaving the directory itself in place.
    pub fn purge_all(&self) -> io::Result<()> {
        fs::remove_dir_all(&self.dir)?;
        fs::create_dir_all(&self.dir)?;
        Ok(())
    }

    fn jpg_path(&self, digest: &str) -> PathBuf {
        self.dir.join(format!("{digest}.jpg"))
    }

    fn key_path(&self, digest: &str) -> PathBuf {
        self.dir.join(format!("{digest}.key"))
    }

    /// `true` iff `<digest>.key` exists, parses as a [`PreviewKey`], and
    /// compares byte-exact to `key`. This is the collision guard: a digest
    /// match with a *different* stored key is treated as a miss.
    fn key_matches(&self, digest: &str, key: &PreviewKey) -> bool {
        let Ok(bytes) = fs::read(self.key_path(digest)) else {
            return false;
        };
        matches!(serde_json::from_slice::<PreviewKey>(&bytes), Ok(parsed) if parsed == *key)
    }

    /// Sets `<digest>.key`'s mtime to now, recording last-access for
    /// eviction (Task 4's LRU reads this). Best-effort: failure to touch
    /// (e.g. the file vanished under us) does not affect the `get` result.
    fn touch_key(&self, digest: &str) {
        let Ok(file) = fs::OpenOptions::new()
            .write(true)
            .open(self.key_path(digest))
        else {
            return;
        };
        let times = fs::FileTimes::new().set_modified(SystemTime::now());
        let _ = file.set_times(times);
    }
}

/// Writes `bytes` to `<path>.tmp` then renames it onto `path`. `rename` is
/// atomic on the same filesystem (and replaces an existing destination on
/// both Unix and Windows), so readers only ever see the old or the fully
/// written new file — never a partial write.
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let tmp_path = PathBuf::from(tmp_name);
    fs::write(&tmp_path, bytes)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some(ext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    /// Unique per-test temp dir under the OS temp dir (no `tempfile` dep —
    /// not currently a workspace dependency, so we avoid introducing it).
    /// Nanosecond timestamp + an atomic counter keep concurrent test
    /// threads from colliding on the same directory.
    fn unique_temp_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ferrolite-previews-test-{label}-{nanos}-{seq}"))
    }

    /// Cleans up a test's temp dir on drop, so failing assertions (which
    /// `panic!` mid-test) don't leak directories on disk.
    struct TestDir(PathBuf);

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn base_key() -> PreviewKey {
        PreviewKey {
            file_size: 12_345_678,
            file_mtime_ns: 1_700_000_000_000_000_000,
            op_stack_hash: 0xdead_beef_cafe_f00d,
            working_space: 2,
            color_profile_hash: 0x1122_3344_5566_7788,
            preview_long_edge: 2048,
            schema_version: 1,
        }
    }

    fn other_key() -> PreviewKey {
        PreviewKey {
            file_size: 99_999,
            ..base_key()
        }
    }

    #[test]
    fn put_then_get_roundtrips() {
        let dir = TestDir(unique_temp_dir("roundtrip"));
        let store = PreviewStore::new(&dir.0).expect("store creates its dir");
        let key = base_key();
        let payload = b"fake jpeg bytes".to_vec();

        store.put(&key, &payload).expect("put succeeds");

        assert!(store.contains(&key));
        assert_eq!(store.get(&key), Some(payload));
    }

    #[test]
    fn get_miss_on_absent() {
        let dir = TestDir(unique_temp_dir("miss-on-absent"));
        let store = PreviewStore::new(&dir.0).expect("store creates its dir");
        let key = base_key();

        assert_eq!(store.get(&key), None);
        assert!(!store.contains(&key));
    }

    #[test]
    fn key_mismatch_on_digest_collision_is_miss() {
        // We don't need a real FNV-64 collision to exercise the guard: the
        // guard only ever compares "the key JSON stored at
        // `<digest>.key`" against "the key passed to `get`/`contains`". So
        // we simulate a collision directly — write a `.jpg` + `.key` pair
        // for `key_b` at `key_b`'s own digest (a normal `put`), confirm
        // `get(&key_b)` hits, then overwrite *only* the `.key` file's
        // contents with a different key's JSON while leaving the filename
        // (i.e. the digest) unchanged. That reproduces exactly what a real
        // digest collision would look like on disk: same digest filename,
        // mismatched stored key. `get(&key_b)` must then miss.
        let dir = TestDir(unique_temp_dir("collision"));
        let store = PreviewStore::new(&dir.0).expect("store creates its dir");
        let key_b = other_key();
        let key_c = base_key();
        let payload = b"payload for key_b's digest".to_vec();

        store.put(&key_b, &payload).expect("put succeeds");
        assert_eq!(store.get(&key_b), Some(payload.clone()));

        let digest = key_b.digest();
        let key_path = dir.0.join(format!("{digest}.key"));
        let mismatched_json = serde_json::to_vec(&key_c).expect("key serializes");
        fs::write(&key_path, mismatched_json).expect("overwrite .key with a different key");

        assert_eq!(store.get(&key_b), None);
        assert!(!store.contains(&key_b));
    }

    #[test]
    fn put_is_atomic_no_partial() {
        let dir = TestDir(unique_temp_dir("atomic"));
        let store = PreviewStore::new(&dir.0).expect("store creates its dir");
        let key = base_key();

        store.put(&key, b"payload").expect("put succeeds");

        let leftover_tmp = fs::read_dir(&dir.0)
            .expect("dir is readable")
            .flatten()
            .any(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| ext == "tmp")
            });
        assert!(!leftover_tmp, "put() must not leave any *.tmp behind");
    }

    #[test]
    fn total_bytes_sums_payloads() {
        let dir = TestDir(unique_temp_dir("total-bytes"));
        let store = PreviewStore::new(&dir.0).expect("store creates its dir");
        let key_a = base_key();
        let key_b = other_key();
        let payload_a = vec![0u8; 100];
        let payload_b = vec![1u8; 250];

        store.put(&key_a, &payload_a).expect("put a succeeds");
        store.put(&key_b, &payload_b).expect("put b succeeds");

        assert_eq!(
            store.total_bytes(),
            (payload_a.len() + payload_b.len()) as u64
        );
    }

    #[test]
    fn purge_all_empties_the_store_but_keeps_the_dir() {
        let dir = TestDir(unique_temp_dir("purge-all"));
        let store = PreviewStore::new(&dir.0).expect("store creates its dir");
        let key = base_key();
        store.put(&key, b"payload").expect("put succeeds");

        store.purge_all().expect("purge succeeds");

        assert!(dir.0.is_dir(), "purge_all must leave the dir in place");
        assert_eq!(store.total_bytes(), 0);
        assert!(!store.contains(&key));
    }

    #[test]
    fn get_touches_key_mtime_on_hit() {
        let dir = TestDir(unique_temp_dir("touch-mtime"));
        let store = PreviewStore::new(&dir.0).expect("store creates its dir");
        let key = base_key();
        store.put(&key, b"payload").expect("put succeeds");

        let digest = key.digest();
        let key_path = dir.0.join(format!("{digest}.key"));
        let before = fs::metadata(&key_path)
            .expect("key file exists")
            .modified()
            .expect("mtime is readable");

        // Push the stored mtime into the past so the touch on `get` is
        // observable even on filesystems with coarse mtime resolution.
        let backdated = before - std::time::Duration::from_secs(60);
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&key_path)
            .expect("key file opens for writing");
        file.set_times(fs::FileTimes::new().set_modified(backdated))
            .expect("mtime is settable on this platform");

        store.get(&key).expect("get hits");

        let after = fs::metadata(&key_path)
            .expect("key file exists")
            .modified()
            .expect("mtime is readable");
        assert!(after > backdated, "get() must touch the .key mtime on hit");
    }
}
