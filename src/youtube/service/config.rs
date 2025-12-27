use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for YouTube service operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YouTubeServiceConfig {
    /// Default cache TTL for search results
    pub search_cache_ttl: Duration,

    /// Default cache TTL for stream URLs
    pub stream_url_cache_ttl: Duration,

    /// Default cache TTL for song info
    pub song_info_cache_ttl: Duration,

    /// Maximum concurrent operations
    pub max_concurrent_operations: usize,

    /// Timeout for individual operations
    pub operation_timeout: Duration,

    /// Enable/disable caching
    pub cache_enabled: bool,
}

impl Default for YouTubeServiceConfig {
    fn default() -> Self {
        Self {
            search_cache_ttl: Duration::from_secs(300), // 5 minutes
            stream_url_cache_ttl: Duration::from_secs(1800), // 30 minutes
            song_info_cache_ttl: Duration::from_secs(3600), // 1 hour
            max_concurrent_operations: 3,
            operation_timeout: Duration::from_secs(30),
            cache_enabled: true,
        }
    }
}