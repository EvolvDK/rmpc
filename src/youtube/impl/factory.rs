use crate::{
    config::Config,
    youtube::{
        cache::{MemoryCacheService, NullCacheService},
        client::{YtDlpClient, YtDlpClientConfig},
        r#impl::service_impl::YouTubeServiceImpl,
        security::rate_limiting::RateLimiterConfig,
        service::{config::YouTubeServiceConfig, CacheService, YouTubeClient, YouTubeService},
    },
};
use anyhow::Result;
use std::{sync::Arc, time::Duration};

/// A factory for creating pre-configured YouTube service instances based on the application configuration.
///
/// This factory is responsible for constructing the entire dependency graph for the YouTube service,
/// including the low-level client and the caching layer, and returning a trait object `Arc<dyn YouTubeService>`
/// to hide the concrete implementation from consumers.
pub struct YouTubeServiceFactory;

impl YouTubeServiceFactory {
    /// Helper function to create a configured YtDlpClient, removing duplication.
    fn create_client(config: &Config) -> Arc<dyn YouTubeClient> {
        let client_config = YtDlpClientConfig {
            ytdlp_path: config
                .youtube_service
                .ytdlp_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "yt-dlp".to_string()),
            extra_args: config.youtube_service.ytdlp_extra_args.clone(),
            operation_timeout: config.youtube_service.operation_timeout,
            rate_limits: vec![
                RateLimiterConfig {
                    requests: config.youtube_service.requests_per_minute as usize,
                    window: Duration::from_secs(60),
                },
                RateLimiterConfig {
                    requests: config.youtube_service.requests_per_hour as usize,
                    window: Duration::from_secs(3600),
                },
            ],
            ..Default::default()
        };
        Arc::new(YtDlpClient::with_config(client_config))
    }
    
    pub fn create_from_config(config: &Config) -> Result<Arc<dyn YouTubeService>> {
        let client = Self::create_client(config);
        let service_config = YouTubeServiceConfig {
            search_cache_ttl: config.youtube_cache_ttl,
            stream_url_cache_ttl: config.youtube_cache_ttl,
            song_info_cache_ttl: config.youtube_cache_ttl,
            operation_timeout: config.youtube_service.operation_timeout,
            cache_enabled: config.youtube_service.cache_enabled,
            ..Default::default()
        };

        let cache: Arc<dyn CacheService> = if service_config.cache_enabled {
            Arc::new(MemoryCacheService::new())
        } else {
            Arc::new(NullCacheService::default())
        };

        let service = YouTubeServiceImpl::new(service_config, client, cache);
        Ok(Arc::new(service))
    }

    /// Creates a service with caching explicitly disabled, ignoring the config setting.
    /// Useful for testing or specific use cases.
    pub fn create_with_null_cache(config: &Config) -> Result<Arc<dyn YouTubeService>> {
        let client = Self::create_client(config);
        let service_config = YouTubeServiceConfig {
            search_cache_ttl: config.youtube_cache_ttl,
            stream_url_cache_ttl: config.youtube_cache_ttl,
            song_info_cache_ttl: config.youtube_cache_ttl,
            operation_timeout: config.youtube_service.operation_timeout,
            cache_enabled: false,
            ..Default::default()
        };
        let cache = Arc::new(NullCacheService::default());

        let service = YouTubeServiceImpl::new(service_config, client, cache);
        Ok(Arc::new(service))
    }
}
