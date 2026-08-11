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
        if let Ok(mut stats) = self.stats.lock() {
            stats.inserts += 1;
            stats.evictions += evictions as u64;
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
}
