use crate::youtube::{error::YouTubeError, CacheStats, ResolvedYouTubeSong};
use anyhow::Result;
use async_trait::async_trait;
use std::{collections::HashMap, time::{Duration, Instant}};

use super::{config::YouTubeServiceConfig, events::YouTubeEventEmitter};

/// Result type for search operations with pagination support
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub songs: Vec<ResolvedYouTubeSong>,
    pub has_more: bool,
    pub continuation_token: Option<String>,
    pub estimated_total: Option<u64>,
}

/// Context for search operations
#[derive(Debug, Clone, Default)]
pub struct SearchContext {
    pub limit: Option<usize>,
    pub continuation_token: Option<String>,
    pub generation: u64,
    pub cache_ttl: Option<Duration>,
}

/// Core YouTube client interface - abstracts external YouTube operations
#[async_trait]
pub trait YouTubeClient: Send + Sync + std::fmt::Debug {
    async fn search(&self, query: &str, context: &SearchContext, emitter: &dyn YouTubeEventEmitter) -> Result<(), YouTubeError>;
    async fn get_stream_url(&self, youtube_id: &str) -> Result<String, YouTubeError>;
    async fn get_song_info(&self, youtube_id: &str) -> Result<Option<ResolvedYouTubeSong>, YouTubeError>;
    async fn get_song_info_batch(&self, youtube_ids: &[String]) -> Result<Vec<ResolvedYouTubeSong>, YouTubeError>;
    async fn health_check(&self) -> Result<(), YouTubeError>;
    fn capabilities(&self) -> YouTubeClientCapabilities;
}

#[derive(Debug, Clone)]
pub struct YouTubeClientCapabilities {
    pub supports_pagination: bool,
    pub max_results_per_request: Option<usize>,
    pub supported_audio_formats: Vec<String>,
    pub rate_limits: Option<RateLimitInfo>,
}

#[derive(Debug, Clone)]
pub struct RateLimitInfo {
    pub requests_per_minute: usize,
    pub requests_per_hour: usize,
}

/// Cache service interface - abstracts caching operations
#[async_trait]
pub trait CacheService: Send + Sync + std::fmt::Debug {
    async fn get_search_results(&self, query: &str) -> Result<Option<SearchResult>>;
    async fn cache_search_results(&self, query: String, results: SearchResult, ttl: Duration) -> Result<()>;
    async fn get_stream_url(&self, youtube_id: &str) -> Result<Option<String>>;
    async fn cache_stream_url(&self, youtube_id: String, url: String, ttl: Duration) -> Result<()>;
    async fn get_song_info(&self, youtube_id: &str) -> Result<Option<ResolvedYouTubeSong>>;
    async fn cache_song_info(&self, youtube_id: String, info: ResolvedYouTubeSong, ttl: Duration) -> Result<()>;
    async fn get_song_info_many(&self, youtube_ids: &[String]) -> Result<HashMap<String, ResolvedYouTubeSong>>;
    async fn cache_song_info_many(&self, songs: &[ResolvedYouTubeSong], ttl: Duration) -> Result<()>;
    async fn cleanup_expired(&self) -> Result<()>;
    async fn get_stats(&self) -> Result<CacheStats>;
    async fn clear(&self) -> Result<()>;
    async fn clear_pattern(&self, pattern: &str) -> Result<()>;
}

/// Main YouTube service interface - high-level operations
#[async_trait]
pub trait YouTubeService: Send + Sync + std::fmt::Debug {
    async fn search(&self, query: &str, context: SearchContext, emitter: &dyn YouTubeEventEmitter) -> Result<(), YouTubeError>;
    async fn get_stream_url(&self, youtube_id: &str) -> Result<String, YouTubeError>;
    async fn get_song_info(&self, youtube_id: &str) -> Result<Option<ResolvedYouTubeSong>, YouTubeError>;
    async fn get_song_info_batch(&self, youtube_ids: &[String]) -> Result<Vec<ResolvedYouTubeSong>, YouTubeError>;
    fn config(&self) -> &YouTubeServiceConfig;
    fn update_config(&mut self, config: YouTubeServiceConfig) -> Result<()>;
    async fn cache_stats(&self) -> Result<CacheStats>;
    async fn cleanup_cache(&self) -> Result<()>;
    async fn health_check(&self) -> Result<ServiceHealthStatus, YouTubeError>;
}

#[derive(Debug, Clone)]
pub struct ServiceHealthStatus {
    pub healthy: bool,
    pub client_healthy: bool,
    pub cache_healthy: bool,
    pub status_details: Vec<String>,
	pub last_check: Instant,
}

impl ServiceHealthStatus {
    pub fn new(client_health: Result<()>, cache_health: Result<()>) -> Self {
        let mut status_details = Vec::new();
        if let Err(e) = &client_health {
            status_details.push(format!("Client health check failed: {}", e));
        }
        if let Err(e) = &cache_health {
            status_details.push(format!("Cache health check failed: {}", e));
        }

        let is_ok = client_health.is_ok() && cache_health.is_ok();
        if is_ok {
            status_details.push("All services healthy.".to_string());
        }

        Self {
            healthy: is_ok,
            client_healthy: client_health.is_ok(),
            cache_healthy: cache_health.is_ok(),
            status_details,
            last_check: Instant::now(),
        }
    }
}
