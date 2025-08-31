use crate::youtube::{
    service::{CacheService, SearchResult},
    CacheStats, ResolvedYouTubeSong,
};
use anyhow::Result;
use async_trait::async_trait;
use lru::LruCache;
use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
struct CacheEntry<T> {
    data: T,
    expires_at: Instant,
}

impl<T> CacheEntry<T> {
    fn new(data: T, ttl: Duration) -> Self { Self { data, expires_at: Instant::now() + ttl } }
    fn is_expired(&self) -> bool { Instant::now() >= self.expires_at }
}

#[derive(Debug)]
pub struct MemoryCacheService {
    search_cache: Arc<RwLock<LruCache<String, CacheEntry<SearchResult>>>>,
    stream_url_cache: Arc<RwLock<LruCache<String, CacheEntry<String>>>>,
    song_info_cache: Arc<RwLock<LruCache<String, CacheEntry<ResolvedYouTubeSong>>>>,
}

impl Default for MemoryCacheService {
    fn default() -> Self { Self::with_capacities(1000, 5000, 10000) }
}

impl MemoryCacheService {
    pub fn new() -> Self { Self::default() }

    // [ADDED] Allow configurable capacities for more flexible setup.
    pub fn with_capacities(search: usize, stream_url: usize, song_info: usize) -> Self {
        Self {
            search_cache: Arc::new(RwLock::new(LruCache::new(NonZeroUsize::new(search).unwrap()))),
            stream_url_cache: Arc::new(RwLock::new(LruCache::new(NonZeroUsize::new(stream_url).unwrap()))),
            song_info_cache: Arc::new(RwLock::new(LruCache::new(NonZeroUsize::new(song_info).unwrap()))),
        }
    }
    
    async fn get_and_promote<T: Clone>(
        cache: &Arc<RwLock<LruCache<String, CacheEntry<T>>>>,
        key: &str,
    ) -> Option<T> {
        let read_guard = cache.read().await;
        if let Some(entry) = read_guard.peek(key) {
            if !entry.is_expired() {
                drop(read_guard);
                let mut write_guard = cache.write().await;
                if let Some(entry) = write_guard.get(key) {
                    return Some(entry.data.clone());
                }
            } else {
                drop(read_guard);
                let mut write_guard = cache.write().await;
                write_guard.pop(key);
                return None;
            }
        }
        None
    }
}

#[async_trait]
impl CacheService for MemoryCacheService {
    // --- Single Getters ---
    async fn get_search_results(&self, query: &str) -> Result<Option<SearchResult>> {
        Ok(Self::get_and_promote(&self.search_cache, query).await)
    }
    async fn get_stream_url(&self, youtube_id: &str) -> Result<Option<String>> {
        Ok(Self::get_and_promote(&self.stream_url_cache, youtube_id).await)
    }
    async fn get_song_info(&self, youtube_id: &str) -> Result<Option<ResolvedYouTubeSong>> {
        Ok(Self::get_and_promote(&self.song_info_cache, youtube_id).await)
    }
    
    // --- Single Setters ---
    async fn cache_search_results(&self, query: String, results: SearchResult, ttl: Duration) -> Result<()> {
        self.search_cache.write().await.put(query, CacheEntry::new(results, ttl));
        Ok(())
    }
    async fn cache_stream_url(&self, youtube_id: String, url: String, ttl: Duration) -> Result<()> {
        self.stream_url_cache.write().await.put(youtube_id, CacheEntry::new(url, ttl));
        Ok(())
    }
    async fn cache_song_info(&self, youtube_id: String, info: ResolvedYouTubeSong, ttl: Duration) -> Result<()> {
        self.song_info_cache.write().await.put(youtube_id, CacheEntry::new(info, ttl));
        Ok(())
    }

    // [ADDED] Efficient batch operations implementation.
    async fn get_song_info_many(&self, youtube_ids: &[String]) -> Result<HashMap<String, ResolvedYouTubeSong>> {
        let mut results = HashMap::new();
        let mut guard = self.song_info_cache.write().await;
        for id in youtube_ids {
            if let Some(entry) = guard.get(id) {
                if !entry.is_expired() {
                    results.insert(id.clone(), entry.data.clone());
                } else {
                    // Eagerly evict during batch read.
                    guard.pop(id);
                }
            }
        }
        Ok(results)
    }

    async fn cache_song_info_many(&self, songs: &[ResolvedYouTubeSong], ttl: Duration) -> Result<()> {
        let mut guard = self.song_info_cache.write().await;
        for song in songs {
            guard.put(song.youtube_id.clone(), CacheEntry::new(song.clone(), ttl));
        }
        Ok(())
    }
    
    // [REFACTORED] This is now a no-op with an explanation.
    async fn cleanup_expired(&self) -> Result<()> {
        // This is intentionally a no-op for this implementation.
        // The MemoryCacheService uses a lazy eviction strategy: expired items are
        // only removed when they are next accessed. A full cleanup scan would
        // be inefficient (O(N)) and require a long-lived write lock,
        // blocking all other cache operations.
        Ok(())
    }

    async fn get_stats(&self) -> Result<CacheStats> {
        Ok(CacheStats {
            searches_count: self.search_cache.read().await.len(),
            stream_urls_count: self.stream_url_cache.read().await.len(),
            song_info_count: self.song_info_cache.read().await.len(),
        })
    }

    async fn clear(&self) -> Result<()> {
        self.search_cache.write().await.clear();
        self.stream_url_cache.write().await.clear();
        self.song_info_cache.write().await.clear();
        Ok(())
    }
    
    async fn clear_pattern(&self, pattern: &str) -> Result<()> {
        // This is inefficient but necessary to fulfill the trait contract.
        {
            let mut guard = self.search_cache.write().await;
            let keys_to_remove: Vec<_> = guard
                .iter()
                .filter(|(k, _)| k.contains(pattern))
                .map(|(k, _)| k.clone())
                .collect();
            for key in keys_to_remove {
                guard.pop(&key);
            }
        }
        {
            let mut guard = self.stream_url_cache.write().await;
            let keys_to_remove: Vec<_> = guard
                .iter()
                .filter(|(k, _)| k.contains(pattern))
                .map(|(k, _)| k.clone())
                .collect();
            for key in keys_to_remove {
                guard.pop(&key);
            }
        }
        {
            let mut guard = self.song_info_cache.write().await;
            let keys_to_remove: Vec<_> = guard
                .iter()
                .filter(|(k, _)| k.contains(pattern))
                .map(|(k, _)| k.clone())
                .collect();
            for key in keys_to_remove {
                guard.pop(&key);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cache_put_and_get() {
        let cache = MemoryCacheService::new();
        let song = ResolvedYouTubeSong { youtube_id: "id1".to_string(), ..Default::default() };
        cache.cache_song_info("id1".to_string(), song.clone(), Duration::from_secs(10)).await.unwrap();
        
        let retrieved = cache.get_song_info("id1").await.unwrap();
        assert_eq!(retrieved, Some(song));
    }

    #[tokio::test]
    async fn test_cache_ttl_expiration() {
        let cache = MemoryCacheService::new();
        let song = ResolvedYouTubeSong { youtube_id: "id1".to_string(), ..Default::default() };
        cache.cache_song_info("id1".to_string(), song.clone(), Duration::from_millis(50)).await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let retrieved = cache.get_song_info("id1").await.unwrap();
        assert!(retrieved.is_none(), "Cache entry should have expired");
    }

    #[tokio::test]
    async fn test_cache_lru_eviction() {
        let cache = MemoryCacheService::with_capacities(1, 1, 2); // song_info cache has size 2
        let song1 = ResolvedYouTubeSong { youtube_id: "id1".to_string(), ..Default::default() };
        let song2 = ResolvedYouTubeSong { youtube_id: "id2".to_string(), ..Default::default() };
        let song3 = ResolvedYouTubeSong { youtube_id: "id3".to_string(), ..Default::default() };
        
        cache.cache_song_info("id1".to_string(), song1.clone(), Duration::from_secs(10)).await.unwrap();
        cache.cache_song_info("id2".to_string(), song2.clone(), Duration::from_secs(10)).await.unwrap();
        cache.cache_song_info("id3".to_string(), song3.clone(), Duration::from_secs(10)).await.unwrap(); // This should evict id1

        assert!(cache.get_song_info("id1").await.unwrap().is_none(), "id1 should be evicted");
        assert!(cache.get_song_info("id2").await.unwrap().is_some());
        assert!(cache.get_song_info("id3").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_batch_operations() {
        let cache = MemoryCacheService::new();
        let songs = vec![
            ResolvedYouTubeSong { youtube_id: "id1".to_string(), ..Default::default() },
            ResolvedYouTubeSong { youtube_id: "id2".to_string(), ..Default::default() },
        ];
        cache.cache_song_info_many(&songs, Duration::from_secs(10)).await.unwrap();

        let ids_to_get = vec!["id1".to_string(), "id3".to_string()]; // id3 is a miss
        let retrieved = cache.get_song_info_many(&ids_to_get).await.unwrap();

        assert_eq!(retrieved.len(), 1);
        assert!(retrieved.contains_key("id1"));
        assert!(!retrieved.contains_key("id3"));
    }
}
