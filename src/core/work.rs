use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use crossbeam::channel::{Receiver, Sender};
use tokio::task::JoinHandle;

use crate::{
    config::{cli_config::CliConfig, Config},
    shared::{
        events::{AppEvent, ClientRequest, WorkDone, WorkRequest},
        lrc::LrcIndex,
        macros::try_skip,
        mpd_query::MpdCommand,
    },
    youtube::{self, YouTubeCache},
};

/// Worker orchestrator - manages work execution and YouTube operations
pub struct WorkerOrchestrator {
    youtube_cache: YouTubeCache,
    youtube_cache_ttl: Duration,
    current_search_handle: Option<JoinHandle<()>>,
}


impl WorkerOrchestrator {
    pub(crate) fn new(config: &Config) -> Self {
        Self {
            youtube_cache: YouTubeCache::new(),
            youtube_cache_ttl: config.youtube_cache_ttl,
            current_search_handle: None,
        }
    }

    /// Handles YouTube search requests with proper cancellation
    async fn handle_youtube_search(
        &mut self,
        query: String,
        generation: u64,
        event_tx: Sender<AppEvent>,
    ) {
        // Cancel previous search if it exists (SRP: single search at a time)
        if let Some(handle) = self.current_search_handle.take() {
            handle.abort();
        }

        let cache_ref = self.youtube_cache.clone();
        let ttl = self.youtube_cache_ttl;

        let new_handle = tokio::spawn(async move {
            if let Err(err) = youtube::search(&cache_ref, &query, generation, ttl, event_tx).await {
                log::error!("YouTube search failed: {}", err);
            }
        });
        self.current_search_handle = Some(new_handle);
    }

    /// Handles non-search work requests
    async fn handle_work_request(
        &self,
        request: WorkRequest,
        client_tx: &Sender<ClientRequest>,
        event_tx: &Sender<AppEvent>,
        config: &CliConfig,
    ) -> Result<()> {
        match request {
            WorkRequest::Command(command) => {
                self.handle_command_request(command, client_tx, event_tx, config).await
            }
            WorkRequest::IndexLyrics { lyrics_dir } => {
                self.handle_index_lyrics_request(lyrics_dir, event_tx).await
            }
            WorkRequest::IndexSingleLrc { path } => {
                self.handle_single_lrc_request(path, event_tx).await
            }
            WorkRequest::GetYouTubeStreamUrl { song, context } => {
                self.handle_stream_url_request(song, context, event_tx).await
            }
            WorkRequest::RefreshYouTubeStream { old_song_id, position, song } => {
                self.handle_stream_refresh_request(old_song_id, position, song, event_tx).await
            }
            WorkRequest::YouTubeGetSongInfo { id } => {
                self.handle_song_info_request(id, event_tx).await
            }
            WorkRequest::YouTubeSearch { .. } => {
                unreachable!("YouTube search should be handled separately")
            }
        }
    }

    /// Handles MPD command execution (SRP: separated command handling)
    async fn handle_command_request(
        &self,
        command: crate::config::cli::Command,
        client_tx: &Sender<ClientRequest>,
        event_tx: &Sender<AppEvent>,
        config: &CliConfig,
    ) -> Result<()> {
        let callback = command.execute(config)?;
        try_skip!(
            client_tx.send(ClientRequest::Command(MpdCommand { callback })),
            "Failed to send client request to complete command"
        );
        event_tx.send(AppEvent::WorkDone(Ok(WorkDone::None)))?;
        Ok(())
    }

    /// Handles lyrics directory indexing (SRP: separated lyrics handling)
    async fn handle_index_lyrics_request(
        &self,
        lyrics_dir: String,
        event_tx: &Sender<AppEvent>,
    ) -> Result<()> {
        let index = LrcIndex::index(&PathBuf::from(lyrics_dir));
        event_tx.send(AppEvent::WorkDone(Ok(WorkDone::LyricsIndexed { index })))?;
        Ok(())
    }

    /// Handles single LRC file indexing (SRP: separated single file handling)
    async fn handle_single_lrc_request(
        &self,
        path: PathBuf,
        event_tx: &Sender<AppEvent>,
    ) -> Result<()> {
        let result = WorkDone::SingleLrcIndexed {
            lrc_entry: LrcIndex::index_single(path)?,
        };
        event_tx.send(AppEvent::WorkDone(Ok(result)))?;
        Ok(())
    }

    /// Handles YouTube stream URL fetching (DIP: depends on cache abstraction)
    async fn handle_stream_url_request(
        &self,
        song: crate::core::data_store::models::YouTubeSong,
        context: Option<crate::shared::events::YouTubeStreamContext>,  // Changed to Option
        event_tx: &Sender<AppEvent>,
    ) -> Result<()> {
        let result = youtube::get_stream_url(&self.youtube_cache, &song.youtube_id, self.youtube_cache_ttl).await;
        
        let work_done = match result {
            Ok(url) => WorkDone::YouTubeStreamUrlReady { url, song, context },
            Err(_) => WorkDone::YouTubeStreamUrlFailed { song, context },
        };
        
        event_tx.send(AppEvent::WorkDone(Ok(work_done)))?;
        Ok(())
    }

    /// Handles YouTube stream refresh (SRP: separated refresh logic)
    async fn handle_stream_refresh_request(
        &self,
        old_song_id: u32,
        position: u32,
        song: crate::core::data_store::models::YouTubeSong,
        event_tx: &Sender<AppEvent>,
    ) -> Result<()> {
        let result = youtube::get_stream_url(&self.youtube_cache, &song.youtube_id, self.youtube_cache_ttl).await;
        
        let work_done = match result {
            Ok(new_url) => WorkDone::YouTubeStreamRefreshed {
                new_url,
                song,
                old_song_id,
                position,
            },
            Err(_) => WorkDone::YouTubeStreamRefreshFailed {
                song_title: song.title,
            },
        };
        
        event_tx.send(AppEvent::WorkDone(Ok(work_done)))?;
        Ok(())
    }

    /// Handles YouTube song info fetching (DIP: depends on cache abstraction)
    async fn handle_song_info_request(
        &self,
        id: String,
        event_tx: &Sender<AppEvent>,
    ) -> Result<()> {
        if let Ok(Some(song)) = youtube::get_song_info(&self.youtube_cache, &id, self.youtube_cache_ttl).await {
            event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSongInfoFetched(song))))?;
        }
        Ok(())
    }
}

/// Initializes the worker thread with proper error handling
pub fn init(
    work_rx: Receiver<WorkRequest>,
    client_tx: Sender<ClientRequest>,
    event_tx: Sender<AppEvent>,
    config: Arc<Config>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("work".to_owned())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime");

            rt.block_on(worker_loop(work_rx, client_tx, event_tx, config));
        })
}

/// Main worker loop with improved separation of concerns
async fn worker_loop(
    work_rx: Receiver<WorkRequest>,
    client_tx: Sender<ClientRequest>,
    event_tx: Sender<AppEvent>,
    config: Arc<Config>,
) {
    let cli_config: CliConfig = config.as_ref().into();
    let mut orchestrator = WorkerOrchestrator::new(&config);

    while let Ok(request) = work_rx.recv() {
        match request {
            WorkRequest::YouTubeSearch { query, generation } => {
                orchestrator
                    .handle_youtube_search(query, generation, event_tx.clone())
                    .await;
            }
            other_request => {
                let client_tx_clone = client_tx.clone();
                let event_tx_clone = event_tx.clone();
                let cli_config_clone = cli_config.clone();

                let youtube_cache_ref = orchestrator.youtube_cache.clone();
                let youtube_cache_ttl = orchestrator.youtube_cache_ttl;

                tokio::spawn(async move {
                    let temp_orchestrator = WorkerOrchestrator {
                        youtube_cache: youtube_cache_ref,
                        youtube_cache_ttl,
                        current_search_handle: None,
                    };

                    if let Err(err) = temp_orchestrator
                        .handle_work_request(
                            other_request,
                            &client_tx_clone,
                            &event_tx_clone,
                            &cli_config_clone,
                        )
                        .await
                    {
                        try_skip!(
                            event_tx_clone.send(AppEvent::WorkDone(Err(err))),
                            "Failed to send work done error notification"
                        );
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_orchestrator_creation() {
        let config = Config::default(); // Assuming Config has Default
        let orchestrator = WorkerOrchestrator::new(&config);
        
        // Verify the orchestrator is properly initialized
        assert!(orchestrator.current_search_handle.is_none());
    }

    // Additional tests would go here for each handler method
}