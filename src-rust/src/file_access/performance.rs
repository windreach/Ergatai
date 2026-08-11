//! Performance optimization for file access control.
//!
//! Provides lock caching, batch operations, and async helpers
//! to reduce SQLite query overhead and improve throughput.

use crate::error::ErgataiError;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// Cached lock information
#[derive(Debug, Clone)]
pub struct CachedLock {
    /// File path
    pub file_path: String,
    /// Lock mode (READ, WRITE, ADMIN)
    pub mode: String,
    /// Token ID
    pub token_id: String,
    /// Agent ID
    pub agent_id: String,
    /// When the cache entry was created
    pub cached_at: Instant,
    /// Time-to-live for this cache entry
    pub ttl: Duration,
}

impl CachedLock {
    /// Check if the cache entry has expired
    pub fn is_expired(&self) -> bool {
        self.cached_at.elapsed() > self.ttl
    }
}

/// Lock cache for reducing SQLite query overhead
pub struct LockCache {
    /// Cache entries (file_path -> CachedLock)
    cache: Arc<Mutex<HashMap<String, CachedLock>>>,
    /// Default TTL for cache entries
    default_ttl: Duration,
    /// Maximum cache size
    max_size: usize,
    /// Cache statistics
    stats: Arc<Mutex<CacheStats>>,
}

/// Cache statistics
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Cache hits
    pub hits: u64,
    /// Cache misses
    pub misses: u64,
    /// Cache evictions
    pub evictions: u64,
    /// Cache inserts
    pub inserts: u64,
}

impl LockCache {
    /// Create a new LockCache
    pub fn new(default_ttl_secs: u64, max_size: usize) -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            default_ttl: Duration::from_secs(default_ttl_secs),
            max_size,
            stats: Arc::new(Mutex::new(CacheStats::default())),
        }
    }

    /// Get a cached lock entry
    pub fn get(&self, file_path: &str) -> Option<CachedLock> {
        // Phase 1: Read under cache lock only
        let result = {
            let cache = match self.cache.lock() {
                Ok(c) => c,
                Err(_) => return None,
            };
            match cache.get(file_path) {
                Some(cached) if !cached.is_expired() => {
                    debug!(file_path = %file_path, "Lock cache hit");
                    Some(cached.clone())
                }
                _ => {
                    debug!(file_path = %file_path, "Lock cache miss");
                    None
                }
            }
        }; // cache lock released here

        // Phase 2: Update stats without holding cache lock
        if let Ok(mut stats) = self.stats.lock() {
            if result.is_some() {
                stats.hits += 1;
            } else {
                stats.misses += 1;
            }
        }

        result
    }

    /// Insert a lock entry into the cache
    pub fn insert(&self, file_path: String, mode: String, token_id: String, agent_id: String) {
        let evictions;

        // Phase 1: Mutate cache under cache lock only
        {
            let mut cache = match self.cache.lock() {
                Ok(c) => c,
                Err(_) => return,
            };

            // Evict expired entries if cache is full
            if cache.len() >= self.max_size {
                let expired_keys: Vec<String> = cache
                    .iter()
                    .filter(|(_, v)| v.is_expired())
                    .map(|(k, _)| k.clone())
                    .collect();

                for key in expired_keys {
                    cache.remove(&key);
                }

                // If still full, evict oldest entries
                if cache.len() >= self.max_size {
                    let mut entries: Vec<_> = cache.iter().map(|(k, v)| (k.clone(), v.cached_at)).collect();
                    entries.sort_by_key(|(_, t)| *t);

                    let to_evict = cache.len() - self.max_size + 1;
                    for (key, _) in entries.into_iter().take(to_evict) {
                        cache.remove(&key);
                    }
                }
            }

            evictions = self.max_size.saturating_sub(cache.len()).min(1); // approximate

            // Insert new entry
            let cached = CachedLock {
                file_path: file_path.clone(),
                mode,
                token_id,
                agent_id,
                cached_at: Instant::now(),
                ttl: self.default_ttl,
            };

            cache.insert(file_path, cached);
        } // cache lock released here

        // Phase 2: Update stats without holding cache lock
        let _ = evictions; // eviction count is approximate; stats update is best-effort
        if let Ok(mut stats) = self.stats.lock() {
            stats.inserts += 1;
        }
    }

    /// Remove a lock entry from the cache
    pub fn remove(&self, file_path: &str) {
        let mut cache = match self.cache.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        cache.remove(file_path);
    }

    /// Batch check if multiple files are in cache (single lock acquisition)
    ///
    /// Returns a map of file_path -> is_cached_and_not_expired
    pub fn batch_contains(&self, file_paths: &[String]) -> HashMap<String, bool> {
        let mut results = HashMap::with_capacity(file_paths.len());

        let cache = match self.cache.lock() {
            Ok(c) => c,
            Err(_) => {
                for file_path in file_paths {
                    results.insert(file_path.clone(), false);
                }
                return results;
            }
        };

        for file_path in file_paths {
            let is_locked = cache
                .get(file_path)
                .map(|c| !c.is_expired())
                .unwrap_or(false);
            results.insert(file_path.clone(), is_locked);
        }

        results
    }

    /// Clear all entries from the cache
    pub fn clear(&self) {
        let mut cache = match self.cache.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        cache.clear();
        info!("Lock cache cleared");
    }

    /// Get cache statistics
    pub fn get_stats(&self) -> CacheStats {
        self.stats.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Clean up expired entries
    pub fn cleanup_expired(&self) -> usize {
        let mut cache = match self.cache.lock() {
            Ok(c) => c,
            Err(_) => return 0,
        };

        let expired_keys: Vec<String> = cache
            .iter()
            .filter(|(_, v)| v.is_expired())
            .map(|(k, _)| k.clone())
            .collect();

        let count = expired_keys.len();
        for key in expired_keys {
            cache.remove(&key);
        }

        if count > 0 {
            debug!(cleaned_up = count, "Cleaned up expired cache entries");
        }

        count
    }
}

/// Batch operations for file locks
pub struct BatchOperations {
    /// Lock cache
    cache: Arc<LockCache>,
}

impl BatchOperations {
    /// Create a new BatchOperations
    pub fn new(cache: Arc<LockCache>) -> Self {
        Self { cache }
    }

    /// Pre-warm the cache with frequently accessed files
    ///
    /// This is useful for reducing latency on common file accesses.
    pub fn prewarm_cache(&self, files: Vec<(String, String, String, String)>) {
        info!(file_count = files.len(), "Pre-warming lock cache");

        for (file_path, mode, token_id, agent_id) in files {
            self.cache.insert(file_path, mode, token_id, agent_id);
        }

        info!("Lock cache pre-warmed");
    }

    /// Batch check if multiple files are locked
    ///
    /// Returns a map of file_path -> is_locked.
    /// Uses a single cache lock acquisition for all paths.
    pub fn batch_check_locked(&self, file_paths: &[String]) -> HashMap<String, bool> {
        self.cache.batch_contains(file_paths)
    }

    /// Batch update cache entries
    pub fn batch_update_cache(&self, updates: Vec<CacheUpdate>) {
        for update in updates {
            match update {
                CacheUpdate::Insert {
                    file_path,
                    mode,
                    token_id,
                    agent_id,
                } => {
                    self.cache.insert(file_path, mode, token_id, agent_id);
                }
                CacheUpdate::Remove(file_path) => {
                    self.cache.remove(&file_path);
                }
            }
        }
    }
}

/// Cache update operation
pub enum CacheUpdate {
    /// Insert a new entry
    Insert {
        file_path: String,
        mode: String,
        token_id: String,
        agent_id: String,
    },
    /// Remove an entry
    Remove(String),
}

/// Async lock request
#[derive(Clone)]
pub struct AsyncLockRequest {
    /// Token ID
    pub token_id: String,
    /// File path
    pub file_path: String,
    /// Lock mode
    pub mode: String,
    /// Priority (higher = more urgent)
    pub priority: u32,
    /// Request timestamp (as unix millis for Clone compatibility)
    pub requested_at_ms: u64,
}

// BinaryHeap is a max-heap: higher priority first; for same priority, earlier request first.
impl PartialEq for AsyncLockRequest {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.requested_at_ms == other.requested_at_ms
    }
}

impl Eq for AsyncLockRequest {}

impl PartialOrd for AsyncLockRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AsyncLockRequest {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first
        self.priority.cmp(&other.priority)
            // For same priority, earlier request first (lower timestamp = greater)
            .then_with(|| other.requested_at_ms.cmp(&self.requested_at_ms))
    }
}

/// Async lock queue for managing concurrent lock requests
pub struct AsyncLockQueue {
    /// Pending requests (BinaryHeap for O(log n) enqueue/dequeue)
    queue: Arc<Mutex<BinaryHeap<AsyncLockRequest>>>,
    /// Maximum queue size
    max_size: usize,
}

impl AsyncLockQueue {
    /// Create a new AsyncLockQueue
    pub fn new(max_size: usize) -> Self {
        Self {
            queue: Arc::new(Mutex::new(BinaryHeap::new())),
            max_size,
        }
    }

    /// Enqueue a lock request — O(log n)
    pub fn enqueue(&self, request: AsyncLockRequest) -> Result<(), ErgataiError> {
        let mut queue = self.queue.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        if queue.len() >= self.max_size {
            return Err(ErgataiError::InvalidArgument(format!(
                "Async lock queue is full (max {})",
                self.max_size
            )));
        }

        queue.push(request);

        debug!(
            queue_size = queue.len(),
            "Enqueued async lock request"
        );

        Ok(())
    }

    /// Dequeue the next lock request — O(log n)
    pub fn dequeue(&self) -> Option<AsyncLockRequest> {
        let mut queue = self.queue.lock().ok()?;
        queue.pop()
    }

    /// Get queue size
    pub fn size(&self) -> usize {
        self.queue.lock().map(|q| q.len()).unwrap_or(0)
    }

    /// Clear the queue
    pub fn clear(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.clear();
            info!("Async lock queue cleared");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_cache_hit() {
        let cache = LockCache::new(60, 1000);
        cache.insert(
            "test.rs".to_string(),
            "WRITE".to_string(),
            "token1".to_string(),
            "agent1".to_string(),
        );

        let cached = cache.get("test.rs");
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().mode, "WRITE");
    }

    #[test]
    fn test_lock_cache_miss() {
        let cache = LockCache::new(60, 1000);
        let cached = cache.get("nonexistent.rs");
        assert!(cached.is_none());
    }

    #[test]
    fn test_lock_cache_expiry() {
        let cache = LockCache::new(0, 1000); // 0 second TTL = immediate expiry
        cache.insert(
            "test.rs".to_string(),
            "WRITE".to_string(),
            "token1".to_string(),
            "agent1".to_string(),
        );

        std::thread::sleep(Duration::from_millis(10));

        let cached = cache.get("test.rs");
        assert!(cached.is_none()); // Should be expired
    }

    #[test]
    fn test_batch_check_locked() {
        let cache = Arc::new(LockCache::new(60, 1000));
        cache.insert(
            "test1.rs".to_string(),
            "WRITE".to_string(),
            "token1".to_string(),
            "agent1".to_string(),
        );

        let batch = BatchOperations::new(cache);
        let results = batch.batch_check_locked(&[
            "test1.rs".to_string(),
            "test2.rs".to_string(),
        ]);

        assert_eq!(results.get("test1.rs"), Some(&true));
        assert_eq!(results.get("test2.rs"), Some(&false));
    }

    #[test]
    fn test_async_lock_queue_priority() {
        let queue = AsyncLockQueue::new(100);

        queue.enqueue(AsyncLockRequest {
            token_id: "token1".to_string(),
            file_path: "test.rs".to_string(),
            mode: "WRITE".to_string(),
            priority: 1,
            requested_at_ms: 0,
        }).unwrap();

        queue.enqueue(AsyncLockRequest {
            token_id: "token2".to_string(),
            file_path: "test.rs".to_string(),
            mode: "WRITE".to_string(),
            priority: 5,
            requested_at_ms: 0,
        }).unwrap();

        queue.enqueue(AsyncLockRequest {
            token_id: "token3".to_string(),
            file_path: "test.rs".to_string(),
            mode: "WRITE".to_string(),
            priority: 3,
            requested_at_ms: 0,
        }).unwrap();

        // Should dequeue in priority order: 5, 3, 1
        let req1 = queue.dequeue().unwrap();
        assert_eq!(req1.priority, 5);

        let req2 = queue.dequeue().unwrap();
        assert_eq!(req2.priority, 3);

        let req3 = queue.dequeue().unwrap();
        assert_eq!(req3.priority, 1);
    }

    #[test]
    fn test_lock_cache_remove() {
        let cache = LockCache::new(60, 1000);
        cache.insert("a.rs".into(), "WRITE".into(), "t1".into(), "ag1".into());
        cache.insert("b.rs".into(), "READ".into(), "t2".into(), "ag2".into());

        assert!(cache.get("a.rs").is_some());
        cache.remove("a.rs");
        assert!(cache.get("a.rs").is_none());
        assert!(cache.get("b.rs").is_some());
    }

    #[test]
    fn test_lock_cache_remove_nonexistent_is_noop() {
        let cache = LockCache::new(60, 1000);
        cache.remove("ghost.rs"); // should not panic
    }

    #[test]
    fn test_lock_cache_clear() {
        let cache = LockCache::new(60, 1000);
        for i in 0..5 {
            cache.insert(format!("f{}.rs", i), "WRITE".into(), "t".into(), "ag".into());
        }
        assert!(cache.get("f0.rs").is_some());

        cache.clear();

        for i in 0..5 {
            assert!(cache.get(&format!("f{}.rs", i)).is_none());
        }
    }

    #[test]
    fn test_lock_cache_get_stats_tracks_hits_misses_inserts() {
        let cache = LockCache::new(60, 1000);

        let s0 = cache.get_stats();
        assert_eq!(s0.hits, 0);
        assert_eq!(s0.misses, 0);
        assert_eq!(s0.inserts, 0);

        cache.insert("a.rs".into(), "WRITE".into(), "t".into(), "ag".into());
        let _ = cache.get("a.rs");      // hit
        let _ = cache.get("miss.rs");   // miss

        let s1 = cache.get_stats();
        assert_eq!(s1.inserts, 1);
        assert_eq!(s1.hits, 1);
        assert_eq!(s1.misses, 1);
    }

    #[test]
    fn test_lock_cache_cleanup_expired() {
        let cache = LockCache::new(0, 1000); // 0s TTL → expired immediately
        cache.insert("a.rs".into(), "W".into(), "t".into(), "ag".into());
        cache.insert("b.rs".into(), "W".into(), "t".into(), "ag".into());

        std::thread::sleep(Duration::from_millis(5));

        let cleaned = cache.cleanup_expired();
        assert_eq!(cleaned, 2);

        assert!(cache.get("a.rs").is_none());
        assert!(cache.get("b.rs").is_none());
    }

    #[test]
    fn test_lock_cache_cleanup_nothing_expired() {
        let cache = LockCache::new(60, 1000);
        cache.insert("a.rs".into(), "W".into(), "t".into(), "ag".into());
        let cleaned = cache.cleanup_expired();
        assert_eq!(cleaned, 0);
        assert!(cache.get("a.rs").is_some());
    }

    #[test]
    fn test_lock_cache_insert_evicts_when_full() {
        let cache = LockCache::new(60, 3); // max 3 entries

        cache.insert("a.rs".into(), "W".into(), "t".into(), "ag".into());
        cache.insert("b.rs".into(), "W".into(), "t".into(), "ag".into());
        cache.insert("c.rs".into(), "W".into(), "t".into(), "ag".into());

        // Insert one more — should evict an older entry to make room
        cache.insert("d.rs".into(), "W".into(), "t".into(), "ag".into());

        // New entry must be present
        assert!(cache.get("d.rs").is_some());
        let stats = cache.get_stats();
        assert_eq!(stats.inserts, 4);
    }

    #[test]
    fn test_batch_operations_prewarm_cache() {
        let cache = Arc::new(LockCache::new(60, 1000));
        let batch = BatchOperations::new(cache.clone());

        batch.prewarm_cache(vec![
            ("a.rs".into(), "WRITE".into(), "t1".into(), "ag1".into()),
            ("b.rs".into(), "READ".into(), "t2".into(), "ag2".into()),
        ]);

        assert!(cache.get("a.rs").is_some());
        assert!(cache.get("b.rs").is_some());
        assert_eq!(cache.get("a.rs").unwrap().mode, "WRITE");
        assert_eq!(cache.get("b.rs").unwrap().mode, "READ");
    }

    #[test]
    fn test_batch_operations_update_cache_insert_and_remove() {
        let cache = Arc::new(LockCache::new(60, 1000));
        let batch = BatchOperations::new(cache.clone());

        cache.insert("old.rs".into(), "W".into(), "t".into(), "ag".into());
        assert!(cache.get("old.rs").is_some());
        assert!(cache.get("new.rs").is_none());

        batch.batch_update_cache(vec![
            CacheUpdate::Insert {
                file_path: "new.rs".into(),
                mode: "READ".into(),
                token_id: "t".into(),
                agent_id: "ag".into(),
            },
            CacheUpdate::Remove("old.rs".into()),
        ]);

        assert!(cache.get("old.rs").is_none());
        assert!(cache.get("new.rs").is_some());
    }

    #[test]
    fn test_async_lock_queue_size() {
        let queue = AsyncLockQueue::new(100);
        assert_eq!(queue.size(), 0);

        queue.enqueue(AsyncLockRequest {
            token_id: "t".into(), file_path: "f".into(), mode: "W".into(),
            priority: 1, requested_at_ms: 0,
        }).unwrap();
        assert_eq!(queue.size(), 1);

        queue.enqueue(AsyncLockRequest {
            token_id: "t2".into(), file_path: "f2".into(), mode: "W".into(),
            priority: 1, requested_at_ms: 0,
        }).unwrap();
        assert_eq!(queue.size(), 2);

        queue.dequeue();
        assert_eq!(queue.size(), 1);
    }

    #[test]
    fn test_async_lock_queue_clear() {
        let queue = AsyncLockQueue::new(100);
        for i in 0..5 {
            queue.enqueue(AsyncLockRequest {
                token_id: format!("t{}", i),
                file_path: format!("f{}.rs", i),
                mode: "W".into(),
                priority: i,
                requested_at_ms: i as u64,
            }).unwrap();
        }
        assert_eq!(queue.size(), 5);

        queue.clear();
        assert_eq!(queue.size(), 0);
        assert!(queue.dequeue().is_none());
    }

    #[test]
    fn test_async_lock_queue_enqueue_when_full_returns_error() {
        let queue = AsyncLockQueue::new(2);
        queue.enqueue(AsyncLockRequest {
            token_id: "t1".into(), file_path: "f1".into(), mode: "W".into(),
            priority: 1, requested_at_ms: 0,
        }).unwrap();
        queue.enqueue(AsyncLockRequest {
            token_id: "t2".into(), file_path: "f2".into(), mode: "W".into(),
            priority: 1, requested_at_ms: 0,
        }).unwrap();

        let result = queue.enqueue(AsyncLockRequest {
            token_id: "t3".into(), file_path: "f3".into(), mode: "W".into(),
            priority: 1, requested_at_ms: 0,
        });
        assert!(matches!(result, Err(ErgataiError::InvalidArgument(_))));
    }

    #[test]
    fn test_async_lock_queue_dequeue_empty_returns_none() {
        let queue = AsyncLockQueue::new(10);
        assert!(queue.dequeue().is_none());
    }

    #[test]
    fn test_async_lock_queue_fifo_within_same_priority() {
        let queue = AsyncLockQueue::new(100);

        // Same priority, different timestamps — earlier timestamp dequeues first
        queue.enqueue(AsyncLockRequest {
            token_id: "first".into(), file_path: "f".into(), mode: "W".into(),
            priority: 5, requested_at_ms: 100,
        }).unwrap();
        queue.enqueue(AsyncLockRequest {
            token_id: "second".into(), file_path: "f".into(), mode: "W".into(),
            priority: 5, requested_at_ms: 200,
        }).unwrap();
        queue.enqueue(AsyncLockRequest {
            token_id: "third".into(), file_path: "f".into(), mode: "W".into(),
            priority: 5, requested_at_ms: 300,
        }).unwrap();

        assert_eq!(queue.dequeue().unwrap().token_id, "first");
        assert_eq!(queue.dequeue().unwrap().token_id, "second");
        assert_eq!(queue.dequeue().unwrap().token_id, "third");
    }

    #[test]
    fn test_async_lock_request_equality() {
        let a = AsyncLockRequest {
            token_id: "t".into(), file_path: "f".into(), mode: "W".into(),
            priority: 3, requested_at_ms: 42,
        };
        let b = AsyncLockRequest {
            token_id: "different".into(), file_path: "different".into(), mode: "R".into(),
            priority: 3, requested_at_ms: 42,
        };
        // Equality is based only on priority + requested_at_ms
        assert!(a == b);
    }

    #[test]
    fn test_cached_lock_is_expired_directly() {
        let fresh = CachedLock {
            file_path: "f".into(), mode: "W".into(), token_id: "t".into(),
            agent_id: "ag".into(), cached_at: Instant::now(),
            ttl: Duration::from_secs(60),
        };
        assert!(!fresh.is_expired());

        let expired = CachedLock {
            file_path: "f".into(), mode: "W".into(), token_id: "t".into(),
            agent_id: "ag".into(),
            cached_at: Instant::now() - Duration::from_secs(10),
            ttl: Duration::from_secs(1),
        };
        assert!(expired.is_expired());
    }

    #[test]
    fn test_batch_contains_distinguishes_cached_vs_missing() {
        let cache = LockCache::new(60, 1000);
        cache.insert("cached.rs".into(), "W".into(), "t".into(), "ag".into());

        let results = cache.batch_contains(&[
            "cached.rs".into(),
            "missing.rs".into(),
            "also-missing.rs".into(),
        ]);

        assert_eq!(results.get("cached.rs"), Some(&true));
        assert_eq!(results.get("missing.rs"), Some(&false));
        assert_eq!(results.get("also-missing.rs"), Some(&false));
    }

    #[test]
    fn test_batch_contains_expired_entry_returns_false() {
        let cache = LockCache::new(0, 1000); // 0s TTL
        cache.insert("expired.rs".into(), "W".into(), "t".into(), "ag".into());
        std::thread::sleep(Duration::from_millis(5));

        let results = cache.batch_contains(&["expired.rs".into()]);
        assert_eq!(results.get("expired.rs"), Some(&false));
    }
}
