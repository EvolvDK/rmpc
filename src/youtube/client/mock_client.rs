// src/youtube/client/mock_client.rs

use crate::youtube::{
    error::YouTubeError,
    service::{
        events::YouTubeEventEmitter, YouTubeClient, SearchContext,
        YouTubeClientCapabilities, RateLimitInfo,
    },
    ResolvedYouTubeSong,
};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A comprehensive mock YouTube client for testing purposes.
#[derive(Debug, Clone, Default)]
pub struct MockYouTubeClient {
    pub search_results: Arc<Mutex<HashMap<String, Vec<ResolvedYouTubeSong>>>>,
    pub stream_urls: Arc<Mutex<HashMap<String, String>>>,
    pub song_info: Arc<Mutex<HashMap<String, ResolvedYouTubeSong>>>,
    pub force_error: Arc<Mutex<Option<YouTubeError>>>,
    pub operation_delay: Option<std::time::Duration>,
    pub call_counts: Arc<Mutex<HashMap<String, usize>>>,
}

impl MockYouTubeClient {
    pub fn new() -> Self {
        Self::default()
    }

    // --- Test Configuration Methods ---
    pub fn add_search_results(&self, query: &str, results: Vec<ResolvedYouTubeSong>) {
        self.search_results.lock().unwrap().insert(query.to_string(), results);
    }
    pub fn add_stream_url(&self, youtube_id: &str, url: &str) {
        self.stream_urls.lock().unwrap().insert(youtube_id.to_string(), url.to_string());
    }
    pub fn add_song_info(&self, youtube_id: &str, info: ResolvedYouTubeSong) {
        self.song_info.lock().unwrap().insert(youtube_id.to_string(), info);
    }
    pub fn force_error(&self, error: Option<YouTubeError>) {
        *self.force_error.lock().unwrap() = error;
    }
    pub fn get_call_count(&self, operation: &str) -> usize {
        self.call_counts.lock().unwrap().get(operation).copied().unwrap_or(0)
    }

    // --- Internal Helpers ---
    fn increment_call_count(&self, operation: &str) {
        let mut counts = self.call_counts.lock().unwrap();
        *counts.entry(operation.to_string()).or_insert(0) += 1;
    }

    async fn apply_delay_and_check_error(&self) -> Result<(), YouTubeError> {
        if let Some(delay) = self.operation_delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(err) = self.force_error.lock().unwrap().clone() {
            return Err(err);
        }
        Ok(())
    }
}

#[async_trait]
impl YouTubeClient for MockYouTubeClient {
    async fn search(&self, query: &str, context: &SearchContext, emitter: &dyn YouTubeEventEmitter) -> Result<(), YouTubeError> {
        self.increment_call_count("search");
        self.apply_delay_and_check_error().await?;

        let results = self.search_results.lock().unwrap().get(query).cloned().unwrap_or_default();

        for song in &results {
            emitter.emit_search_result(song.clone(), context.generation).map_err(Into::into)?;
        }
        emitter.emit_search_complete(context.generation, results.len()).map_err(Into::into)?;

        Ok(())
    }

    async fn get_stream_url(&self, youtube_id: &str) -> Result<String, YouTubeError> {
        self.increment_call_count("get_stream_url");
        self.apply_delay_and_check_error().await?;

        self.stream_urls
            .lock()
            .unwrap()
            .get(youtube_id)
            .cloned()
            .ok_or(YouTubeError::VideoUnavailable)
    }

    async fn get_song_info(&self, youtube_id: &str) -> Result<Option<ResolvedYouTubeSong>, YouTubeError> {
        self.increment_call_count("get_song_info");
        self.apply_delay_and_check_error().await?;

        Ok(self.song_info.lock().unwrap().get(youtube_id).cloned())
    }

    async fn get_song_info_batch(&self, youtube_ids: &[String]) -> Result<Vec<ResolvedYouTubeSong>, YouTubeError> {
        self.increment_call_count("get_song_info_batch");
        self.apply_delay_and_check_error().await?;

        let info_map = self.song_info.lock().unwrap();
        let results = youtube_ids
            .iter()
            .filter_map(|id| info_map.get(id).cloned())
            .collect();
        Ok(results)
    }

    async fn health_check(&self) -> Result<(), YouTubeError> {
        self.increment_call_count("health_check");
        self.apply_delay_and_check_error().await
    }

    fn capabilities(&self) -> YouTubeClientCapabilities {
        YouTubeClientCapabilities {
            supports_pagination: false,
            max_results_per_request: Some(100),
            supported_audio_formats: vec![
                "mp3".into(),
                "m4a".into(),
                "opus".into(),
            ],
            rate_limits: Some(RateLimitInfo {
                requests_per_minute: 60,
                requests_per_hour: 1000,
            }),
        }
    }
}
