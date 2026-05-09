//! Process-global content cache for the `read` tool.
//!
//! Within a single agent run, the model frequently reads the same file
//! multiple times (e.g., once for context, then again to verify after an
//! edit). Disk + UTF-8 reparsing cost adds up. This cache memoizes by
//! `(path, mtime, size)` so a stale read can never sneak in - any change
//! invalidates the entry on the next call's metadata check.
//!
//! When `edit` writes to a path it explicitly invalidates so the next read
//! goes through to disk.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone)]
struct Entry {
    mtime: u64,
    size: u64,
    content: String,
}

static CACHE: Lazy<Mutex<HashMap<PathBuf, Entry>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Hits, misses, invalidations. Useful for `--inspect-prompt`-style introspection.
#[derive(Default, Debug, Clone, Copy)]
pub struct Stats { pub hits: u64, pub misses: u64, pub invalidations: u64 }
static STATS: Lazy<Mutex<Stats>> = Lazy::new(|| Mutex::new(Stats::default()));

/// Try the cache. On hit, returns the content; on miss, returns None.
/// `mtime` and `size` come from a fresh `metadata()` call by the caller.
pub fn get(path: &Path, mtime: u64, size: u64) -> Option<String> {
    let key = path.to_path_buf();
    let cache = CACHE.lock().ok()?;
    let entry = cache.get(&key)?;
    if entry.mtime == mtime && entry.size == size {
        if let Ok(mut s) = STATS.lock() { s.hits += 1; }
        Some(entry.content.clone())
    } else {
        // Stale. Drop the lock so put() can grab it cleanly.
        None
    }
}

pub fn put(path: &Path, mtime: u64, size: u64, content: String) {
    let key = path.to_path_buf();
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(key, Entry { mtime, size, content });
        if let Ok(mut s) = STATS.lock() { s.misses += 1; }
    }
}

/// Drop a single path from the cache. Called by `edit` after it writes so
/// the next read of that path doesn't return pre-edit content.
pub fn invalidate(path: &Path) {
    let key = path.to_path_buf();
    if let Ok(mut cache) = CACHE.lock() {
        if cache.remove(&key).is_some() {
            if let Ok(mut s) = STATS.lock() { s.invalidations += 1; }
        }
    }
}

/// Clear the entire cache. Used by tests.
#[allow(dead_code)]
pub fn clear() {
    if let Ok(mut cache) = CACHE.lock() { cache.clear(); }
    if let Ok(mut s) = STATS.lock() { *s = Stats::default(); }
}

pub fn stats() -> Stats {
    STATS.lock().map(|s| *s).unwrap_or_default()
}

/// Number of currently-cached entries (paths). Useful for runtime visibility.
pub fn len() -> usize {
    CACHE.lock().map(|c| c.len()).unwrap_or(0)
}

/// Sum of bytes held in cached String content. Approximation of memory cost.
pub fn bytes_held() -> u64 {
    CACHE.lock().map(|c| c.values().map(|e| e.content.len() as u64).sum()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize cache tests since they all touch process-global state
    /// (the cache map and the stats counter). Without this lock, one
    /// test's `clear()` can land between another test's `put` and `get`.
    static TEST_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

    #[test]
    fn miss_then_hit_returns_cached_content() {
        let _g = TEST_LOCK.lock().unwrap();
        clear();
        let p = std::env::temp_dir().join(format!("mlx-readcache-{}.txt", std::process::id()));
        std::fs::write(&p, "alpha\n").unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        let mtime = meta.modified().unwrap()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let size = meta.len();

        assert!(get(&p, mtime, size).is_none(), "fresh cache should miss");
        put(&p, mtime, size, "alpha\n".into());
        assert_eq!(get(&p, mtime, size).as_deref(), Some("alpha\n"), "should hit");

        let s = stats();
        assert!(s.hits >= 1);
        assert!(s.misses >= 1);

        let _ = std::fs::remove_file(&p);
        clear();
    }

    #[test]
    fn mtime_change_invalidates_implicitly() {
        let _g = TEST_LOCK.lock().unwrap();
        clear();
        let p = std::env::temp_dir().join(format!("mlx-readcache-mt-{}.txt", std::process::id()));
        std::fs::write(&p, "before\n").unwrap();
        let m1 = std::fs::metadata(&p).unwrap();
        let mt1 = m1.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let sz1 = m1.len();
        put(&p, mt1, sz1, "before\n".into());
        assert!(get(&p, mt1, sz1).is_some());

        // Different mtime -> miss (cached entry is stale relative to caller).
        assert!(get(&p, mt1 + 5, sz1).is_none());
        // Different size -> also miss.
        assert!(get(&p, mt1, sz1 + 10).is_none());

        let _ = std::fs::remove_file(&p);
        clear();
    }

    #[test]
    fn explicit_invalidate_drops_entry() {
        let _g = TEST_LOCK.lock().unwrap();
        clear();
        let p = std::env::temp_dir().join(format!("mlx-readcache-inv-{}.txt", std::process::id()));
        put(&p, 100, 5, "hello".into());
        assert!(get(&p, 100, 5).is_some());
        invalidate(&p);
        assert!(get(&p, 100, 5).is_none(), "post-invalidate should miss");
        let s = stats();
        assert!(s.invalidations >= 1);
        clear();
    }
}
