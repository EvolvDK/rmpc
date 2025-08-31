use crate::youtube::{
	error::YouTubeError,
    security::validation::{validate_and_sanitize_query, validate_youtube_id, ValidationError},
    service::{
        CacheService, SearchContext, SearchResult, ServiceHealthStatus, YouTubeClient,
        YouTubeEventEmitter, YouTubeService, YouTubeServiceConfig,
    },
    CacheStats, ResolvedYouTubeSong,
};
use anyhow::Result;
use async_trait::async_trait;
use futures::future::try_join_all;
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc, Mutex};
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct YouTubeServiceImpl {
    config: YouTubeServiceConfig,
    client: Arc<dyn YouTubeClient>,
    cache: Arc<dyn CacheService>,
    operation_semaphore: Arc<Semaphore>,
}

impl YouTubeServiceImpl {
    pub fn new(
        config: YouTubeServiceConfig,
        client: Arc<dyn YouTubeClient>,
        cache: Arc<dyn CacheService>,
    ) -> Self {
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_operations));
        Self { config, client, cache, operation_semaphore: semaphore }
    }

    async fn execute_with_limits<F, T>(&self, operation: F) -> Result<T, YouTubeError>
    where F: std::future::Future<Output = Result<T, YouTubeError>> {
        let _permit = self.operation_semaphore.acquire().await
            .map_err(|_| YouTubeError::Internal("Semaphore closed".to_string()))?;
        tokio::time::timeout(self.config.operation_timeout, operation)
            .await
            .map_err(|_| YouTubeError::Timeout(self.config.operation_timeout))?
    }
}

struct CachingEmitter<'a> {
    original_emitter: &'a dyn YouTubeEventEmitter,
    results_aggregator: Arc<Mutex<Vec<ResolvedYouTubeSong>>>,
}

#[async_trait]
impl YouTubeEventEmitter for CachingEmitter<'_> {
    fn emit_search_result(&self, song: ResolvedYouTubeSong, generation: u64) -> Result<()> {
        // Forward to the original emitter.
        self.original_emitter
            .emit_search_result(song.clone(), generation)?;
        // Also, add the result to our internal list for caching.
        // `try_lock` is used to avoid deadlocks in exotic scenarios, though a blocking lock is likely fine.
        self.results_aggregator.try_lock().unwrap().push(song);
        Ok(())
    }
    fn emit_search_complete(&self, generation: u64, total_results: usize) -> Result<()> {
        self.original_emitter
            .emit_search_complete(generation, total_results)
    }
    fn emit_error(&self, error: anyhow::Error, operation: &str) -> Result<()> {
        self.original_emitter.emit_error(error, operation)
    }
}

#[async_trait]
impl YouTubeService for YouTubeServiceImpl {
    async fn search(
        &self,
        query: &str,
        context: SearchContext,
        emitter: &dyn YouTubeEventEmitter,
    ) -> Result<(), YouTubeError> {
        let sanitized_query = validate_and_sanitize_query(query)?;

        // 1. Check cache first.
        if self.config.cache_enabled {
            if let Some(cached) = self.cache.get_search_results(&sanitized_query).await? {
                // Emit cached results.
                for song in cached.songs {
                    emitter.emit_search_result(song, context.generation)?;
                }
                emitter.emit_search_complete(context.generation, 0)?; // Note: total results might be unknown here.
                return Ok(());
            }
        }

        // 2. If cache miss, execute the client call, aggregating results for caching.
        let aggregated_results = Arc::new(Mutex::new(Vec::new()));
        let caching_emitter = CachingEmitter {
            original_emitter: emitter,
            results_aggregator: Arc::clone(&aggregated_results),
        };

        let search_result = self
            .execute_with_limits(self.client.search(
                &sanitized_query,
                &context,
                &caching_emitter,
            ))
            .await;

        // 3. After the search, cache the aggregated results.
        if self.config.cache_enabled {
            let songs_to_cache = aggregated_results.lock().await;
            if !songs_to_cache.is_empty() {
                let search_result_to_cache = SearchResult {
                    songs: songs_to_cache.clone(),
                    has_more: false, // This would need to be passed from the client if pagination is supported
                    continuation_token: None,
                    estimated_total: Some(songs_to_cache.len() as u64),
                };
                self.cache
                    .cache_search_results(
                        sanitized_query,
                        search_result_to_cache,
                        self.config.search_cache_ttl,
                    )
                    .await?;
            }
        }

        search_result
    }
    
    async fn get_stream_url(&self, youtube_id: &str) -> Result<String, YouTubeError> {
		validate_youtube_id(youtube_id)?;

        if self.config.cache_enabled {
            if let Some(cached) = self.cache.get_stream_url(youtube_id).await? {
                return Ok(cached);
            }
        }
        
        let url = self.execute_with_limits(self.client.get_stream_url(youtube_id)).await?;

        if self.config.cache_enabled {
            self.cache.cache_stream_url(youtube_id.to_string(), url.clone(), self.config.stream_url_cache_ttl).await?;
        }
        Ok(url)
    }
    
    async fn get_song_info(&self, youtube_id: &str) -> Result<Option<ResolvedYouTubeSong>, YouTubeError> {
        validate_youtube_id(youtube_id)?;
        if self.config.cache_enabled {
            if let Some(cached) = self.cache.get_song_info(youtube_id).await? {
                return Ok(Some(cached));
            }
        }

        let info = self.execute_with_limits(self.client.get_song_info(youtube_id)).await?;

        if self.config.cache_enabled {
            if let Some(ref song_info) = info {
                self.cache.cache_song_info(youtube_id.to_string(), song_info.clone(), self.config.song_info_cache_ttl).await?;
            }
        }
        Ok(info)
    }
    
    /// Retrieves song information for a batch of YouTube IDs, prioritizing cache.
    async fn get_song_info_batch(
        &self,
        youtube_ids: &[String],
    ) -> Result<Vec<ResolvedYouTubeSong>, YouTubeError> {
        if youtube_ids.is_empty() {
            return Ok(Vec::new());
        }

        // 1. Validate all IDs upfront.
        for id in youtube_ids {
            validate_youtube_id(id)?;
        }

        let mut final_results_map = std::collections::HashMap::new();
        let mut ids_to_fetch = Vec::new();

        if self.config.cache_enabled {
            final_results_map = self.cache.get_song_info_many(youtube_ids).await?;
            // Identify cache misses.
            ids_to_fetch.extend(
                youtube_ids.iter().filter(|id| !final_results_map.contains_key(*id)).cloned(),
            );
        } else {
            ids_to_fetch.extend_from_slice(youtube_ids);
        }
        
        // Dedup is still a good idea in case the input slice has duplicates.
        ids_to_fetch.sort();
        ids_to_fetch.dedup();

        if !ids_to_fetch.is_empty() {
            let fetched_songs = self.execute_with_limits(self.client.get_song_info_batch(&ids_to_fetch)).await?;

            if self.config.cache_enabled && !fetched_songs.is_empty() {
                self.cache.cache_song_info_many(&fetched_songs, self.config.song_info_cache_ttl).await?;
            }

            for song in fetched_songs {
                final_results_map.insert(song.youtube_id.clone(), song);
            }
        }

        // 6. Aggregate results in the original order.
        Ok(youtube_ids.iter().filter_map(|id| final_results_map.get(id).cloned()).collect())
    }
    
    fn config(&self) -> &YouTubeServiceConfig { &self.config }
    
    fn update_config(&mut self, config: YouTubeServiceConfig) -> Result<(), anyhow::Error> {
        if config.max_concurrent_operations != self.config.max_concurrent_operations {
            self.operation_semaphore = Arc::new(Semaphore::new(config.max_concurrent_operations));
        }
        self.config = config;
        Ok(())
    }
    
    async fn cache_stats(&self) -> Result<CacheStats, anyhow::Error> { self.cache.get_stats().await }
    
    async fn cleanup_cache(&self) -> Result<(), anyhow::Error> { self.cache.cleanup_expired().await }
    
    async fn health_check(&self) -> Result<ServiceHealthStatus, YouTubeError> {
        let client_health_fut = self.client.health_check();
        let cache_health_fut = self.cache.get_stats();

        // Run checks in parallel
        let (client_health, cache_health) =
            tokio::join!(client_health_fut, cache_health_fut);

        Ok(ServiceHealthStatus::new(
            client_health,
            cache_health.map(|_| ()).map_err(YouTubeError::from),
        ))
    }
}
