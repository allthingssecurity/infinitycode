use std::collections::HashMap;
use std::sync::Mutex;

/// LRU dentry cache: (parent_ino, name) -> ino.
///
/// Simple bounded HashMap with no eviction strategy beyond capacity check.
/// This is adequate for typical agent workloads with <10K files.
pub struct DentryCache {
    inner: Mutex<CacheInner>,
}

struct CacheInner {
    map: HashMap<(i64, String), i64>,
    capacity: usize,
}

impl DentryCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                map: HashMap::with_capacity(capacity),
                capacity,
            }),
        }
    }

    /// Look up an inode by parent + name.
    pub fn get(&self, parent_ino: i64, name: &str) -> Option<i64> {
        let inner = self.inner.lock().unwrap();
        inner.map.get(&(parent_ino, name.to_string())).copied()
    }

    /// Insert a dentry into the cache.
    pub fn insert(&self, parent_ino: i64, name: String, ino: i64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.map.len() >= inner.capacity {
            // Simple eviction: clear everything. Fine for agent workloads.
            inner.map.clear();
        }
        inner.map.insert((parent_ino, name), ino);
    }

    /// Remove a specific entry.
    pub fn remove(&self, parent_ino: i64, name: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.map.remove(&(parent_ino, name.to_string()));
    }

    /// Clear the entire cache.
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.map.clear();
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_cache_ops() {
        let cache = DentryCache::new(100);
        assert!(cache.is_empty());

        cache.insert(1, "hello.txt".into(), 2);
        assert_eq!(cache.get(1, "hello.txt"), Some(2));
        assert_eq!(cache.get(1, "other.txt"), None);

        cache.remove(1, "hello.txt");
        assert_eq!(cache.get(1, "hello.txt"), None);
    }

    #[test]
    fn eviction_on_capacity() {
        let cache = DentryCache::new(2);
        cache.insert(1, "a".into(), 10);
        cache.insert(1, "b".into(), 11);
        assert_eq!(cache.len(), 2);

        // Third insert triggers clear + insert
        cache.insert(1, "c".into(), 12);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(1, "c"), Some(12));
    }
}
