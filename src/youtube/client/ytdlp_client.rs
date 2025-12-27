// src/youtube/client/ytdlp_client.rs

use crate::youtube::{
    parse_song_info_json,
    security::{
        command_security::SecureYtDlpCommand,
        rate_limiting::{check_global_rate_limit, RateLimiterConfig},
        validation::{validate_and_sanitize_query, validate_youtube_id}
    },
    service::{
        RateLimitInfo, SearchContext, SearchResult, YouTubeClient, YouTubeClientCapabilities, YouTubeEventEmitter,
    },
    ResolvedYouTubeSong,
    error::YouTubeError,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct YtDlpClientConfig {
    pub ytdlp_path: String,
    pub max_results: usize,
    pub operation_timeout: Duration,
    pub extra_args: Vec<String>,
    pub verbose: bool,
    pub rate_limit_key: String,
    pub rate_limits: Vec<RateLimiterConfig>,
}

impl Default for YtDlpClientConfig {
    fn default() -> Self {
        Self {
            ytdlp_path: "yt-dlp".to_string(),
            max_results: 100,
            operation_timeout: Duration::from_secs(30),
            extra_args: vec![],
            verbose: false,
            rate_limit_key: "default".to_string(),
            rate_limits: vec![
                RateLimiterConfig { requests: 10, window: Duration::from_secs(60) },  // 10 per minute
                RateLimiterConfig { requests: 100, window: Duration::from_secs(3600) }, // 100 per hour
            ],
        }
    }
}

#[derive(Debug)]
pub struct YtDlpClient {
    config: YtDlpClientConfig,
    command_executor: SecureYtDlpCommand,
}

impl YtDlpClient {
    pub fn new() -> Self {
        Self::with_config(YtDlpClientConfig::default())
    }

    pub fn with_config(config: YtDlpClientConfig) -> Self {
        Self {
            command_executor: SecureYtDlpCommand::new()
                .with_timeout(config.operation_timeout)
                .with_ytdlp_path(&config.ytdlp_path)
                .with_extra_args(&config.extra_args)
                .with_verbose(config.verbose),
            config,
        }
    }

    fn check_rate_limit(&self) -> Result<(), YouTubeError> {
        if !self.config.rate_limits.is_empty() {
            if !check_global_rate_limit(&self.config.rate_limit_key, &self.config.rate_limits) {
                return Err(YouTubeError::RateLimitExceeded);
            }
        }
        Ok(())
    }

    async fn execute_get_stream_url_operation(&self, youtube_id: &str) -> Result<String, YouTubeError> {
        self.check_rate_limit()?;
        self.command_executor.get_stream_url(youtube_id).await
    }

    async fn execute_get_song_info_operation(&self, youtube_id: &str) -> Result<Option<ResolvedYouTubeSong>, YouTubeError> {
        self.check_rate_limit()?;
        self.command_executor.get_song_info(youtube_id).await
    }

    async fn execute_get_song_info_batch_operation(&self, youtube_ids: &[String]) -> Result<Vec<ResolvedYouTubeSong>, YouTubeError> {
        self.check_rate_limit()?;
        self.command_executor.get_song_info_batch(youtube_ids).await
    }
}

impl Default for YtDlpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl YouTubeClient for YtDlpClient {
    async fn search(&self, query: &str, context: &SearchContext, emitter: &dyn YouTubeEventEmitter) -> Result<(), YouTubeError> {
        let max_results = context
            .limit
            .unwrap_or(self.config.max_results)
            .min(self.config.max_results);

        // This now calls a modified command executor that parses streaming JSON.
        self.command_executor
            .execute_search_streaming(query, max_results, context.generation, emitter)
            .await
    }

    async fn get_stream_url(&self, youtube_id: &str) -> Result<String, YouTubeError> {
        self.execute_get_stream_url_operation(youtube_id).await
    }

    async fn get_song_info(&self, youtube_id: &str) -> Result<Option<ResolvedYouTubeSong>, YouTubeError> {
        self.execute_get_song_info_operation(youtube_id).await
    }
    
    async fn get_song_info_batch(&self, youtube_ids: &[String]) -> Result<Vec<ResolvedYouTubeSong>, YouTubeError> {
        self.execute_get_song_info_batch_operation(youtube_ids).await
    }

    async fn health_check(&self) -> Result<(), YouTubeError> {
        self.command_executor
            .health_check()
            .await
    }

    fn capabilities(&self) -> YouTubeClientCapabilities {
        let requests_per_minute = self.config.rate_limits.iter()
            .find(|c| c.window == Duration::from_secs(60))
            .map_or(0, |c| c.requests);
        let requests_per_hour = self.config.rate_limits.iter()
            .find(|c| c.window == Duration::from_secs(3600))
            .map_or(0, |c| c.requests);
            
        YouTubeClientCapabilities {
            supports_pagination: false,
            max_results_per_request: Some(self.config.max_results),
            supported_audio_formats: vec![
                "m4a".into(), "opus".into(), "wav".into(), "flac".into(),
                "mp4a".into(), "ogg".into(), "webm".into(), "aac".into(), "mp3".into(),
            ],
            rate_limits: Some(RateLimitInfo {
                requests_per_minute,
                requests_per_hour,
            }),
        }
    }
}
