use crate::youtube::{
    service::{CacheService, SearchResult},
    CacheStats, ResolvedYouTubeSong,
};
use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use std::collections::HashMap;

/// A no-op cache implementation for testing or for disabling caching.
#[derive(Debug, Default)]
pub struct NullCacheService;

#[async_trait]
impl CacheService for NullCacheService {
    async fn get_search_results(&self, _: &str) -> Result<Option<SearchResult>> { Ok(None) }
    async fn cache_search_results(&self, _: String, _: SearchResult, _: Duration) -> Result<()> { Ok(()) }
    async fn get_stream_url(&self, _: &str) -> Result<Option<String>> { Ok(None) }
    async fn cache_stream_url(&self, _: String, _: String, _: Duration) -> Result<()> { Ok(()) }
    async fn get_song_info(&self, _: &str) -> Result<Option<ResolvedYouTubeSong>> { Ok(None) }
    async fn cache_song_info(&self, _: String, _: ResolvedYouTubeSong, _: Duration) -> Result<()> { Ok(()) }
    
    // Batch operations return empty results.
    async fn get_song_info_many(&self, _: &[String]) -> Result<HashMap<String, ResolvedYouTubeSong>> {
        Ok(HashMap::new())
    }
    async fn cache_song_info_many(&self, _: &[ResolvedYouTubeSong], _: Duration) -> Result<()> {
        Ok(())
    }

    // --- Management ---
    async fn cleanup_expired(&self) -> Result<()> { Ok(()) }
    async fn get_stats(&self) -> Result<CacheStats> { Ok(CacheStats::default()) }
    async fn clear(&self) -> Result<()> { Ok(()) }
    async fn clear_pattern(&self, _: &str) -> Result<()> { Ok(()) }
}
