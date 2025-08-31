use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use crossbeam::channel::{Receiver, Sender};
use tokio::task::JoinHandle;

use crate::{
    config::{cli_config::CliConfig, Config},
    core::data_store::models::YouTubeSong,
    shared::{
        events::{YouTubeStreamContext, AppEvent, ClientRequest, WorkDone, WorkRequest, IdentifiedYouTubeSong},
        lrc::LrcIndex,
        macros::try_skip,
        mpd_query::MpdCommand,
    },
    youtube::{self, service::{YouTubeService, events::YouTubeEventEmitter}, error::YouTubeError},
};

/// Worker orchestrator - manages all background work execution
pub struct WorkerOrchestrator {
    youtube_service: Arc<dyn YouTubeService>,
    current_search_handle: Option<JoinHandle<()>>,
}

impl WorkerOrchestrator {
    pub(crate) fn new(youtube_service: Arc<dyn YouTubeService>) -> Self {
        Self {
            youtube_service,
            current_search_handle: None,
        }
    }

    /// Handles all incoming work requests by dispatching to the correct async handler.
    async fn handle_work_request(
        &mut self,
        request: WorkRequest,
        client_tx: Sender<ClientRequest>,
        event_tx: Sender<AppEvent>,
        config: &CliConfig,
    ) {
        let result = match request {
            WorkRequest::YouTubeSearch { query, generation } => {
                self.handle_youtube_search(query, generation, event_tx).await;
                return; // Search is fire-and-forget with its own event emitters.
            }
            WorkRequest::Command(command) => {
                self.handle_command_request(command, &client_tx, &event_tx, config).await
            }
            WorkRequest::IndexLyrics { lyrics_dir } => {
                self.handle_index_lyrics_request(lyrics_dir, &event_tx).await
            }
            WorkRequest::IndexSingleLrc { path } => {
                self.handle_single_lrc_request(path, &event_tx).await
            }
            WorkRequest::GetYouTubeStreamUrl { song, context } => {
                self.handle_stream_url_request(song, context, event_tx.clone()).await
            }
            WorkRequest::YouTubeGetSongInfo { id, context } => {
                self.handle_song_info_request(id, context, &event_tx).await
            }
        };

        if let Err(err) = result {
            try_skip!(
                event_tx.send(AppEvent::WorkDone(Err(err))),
                "Failed to send work done error"
            );
        }
    }

    /// Handles YouTube search requests with cancellation.
    async fn handle_youtube_search(
        &mut self,
        query: String,
        generation: u64,
        event_tx: Sender<AppEvent>,
    ) {
        if let Some(handle) = self.current_search_handle.take() {
            handle.abort();
        }

        let service = Arc::clone(&self.youtube_service);
        let new_handle = tokio::spawn(async move {
            let emitter = youtube::integration::CrossbeamEventEmitter::new(event_tx);
            let context = youtube::service::SearchContext {
                generation,
                ..Default::default()
            };
            if let Err(e) = service.search(&query, context, &emitter).await {
                log::error!("YouTube search failed: {}", e);
                let _ = emitter.emit_error(e.into(), "search");
            }
        });
        self.current_search_handle = Some(new_handle);
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

    /// Fetches a YouTube stream URL using the abstracted service. Handles both new and refresh requests.
    async fn handle_stream_url_request(
        &self,
        song_to_refresh: IdentifiedYouTubeSong,
        context: Option<YouTubeStreamContext>,
        event_tx: Sender<AppEvent>,
    ) -> Result<()> {
        let song_result: Result<YouTubeSong, YouTubeError> = match &song_to_refresh {
            IdentifiedYouTubeSong::Full(song) => Ok(song.clone()),
            IdentifiedYouTubeSong::IdOnly(id) => {
                log::info!("Metadata for song '{}' is missing, fetching for refresh...", id);
                self.youtube_service
                    .get_song_info(id)
                    .await
                    .and_then(|opt| opt.ok_or(YouTubeError::VideoUnavailable))
                    .map(Into::into)
            }
        };
        let full_song = match song_result {
            Ok(song) => song,
            Err(e) => {
                log::error!("Failed to fetch metadata during refresh: {:?}", e);
                let work_done = WorkDone::YouTubeStreamUrlFailed {
                    song: song_to_refresh, // Send back the original IdOnly variant.
                    context,
                    error: e,
                };
                event_tx.send(AppEvent::WorkDone(Ok(work_done)))?;
                return Ok(());
            }
        };
        let result: Result<String, YouTubeError> = self.youtube_service.get_stream_url(&full_song.youtube_id).await;
        let work_done = match result {
            Ok(url) => WorkDone::YouTubeStreamUrlReady { url, song: full_song, context },
            Err(e) => {
                log::warn!("Failed to get stream URL for '{}': {:?}", full_song.title, e);
                WorkDone::YouTubeStreamUrlFailed { song: IdentifiedYouTubeSong::Full(full_song), context, error: e }
            }
        };
        event_tx.send(AppEvent::WorkDone(Ok(work_done)))?;
        Ok(())
    }

    /// Handles YouTube song info fetching (DIP: depends on cache abstraction)
    async fn handle_song_info_request(
        &self,
        id: String,
        context: Option<YouTubeStreamContext>,
        event_tx: &Sender<AppEvent>,
    ) -> Result<()> {
        match self.youtube_service.get_song_info(&id).await {
            Ok(Some(song)) => {
                event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSongInfoFetched {
                    song: song.into(),
                    context,
                })))?;
            }
            Ok(None) => {
                // Explicitly signal that the video could not be found.
                let error = YouTubeError::VideoUnavailable;
                event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSongInfoFailed {
                    id,
                    error,
                    context,
                })))?;
            }
            Err(e) => {
                // Propagate other errors (network, command failed, etc.).
                event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSongInfoFailed {
                    id,
                    error: e.into(),
                    context,
                })))?;
            }
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
    youtube_service: Arc<dyn YouTubeService>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("work".to_owned())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime");

            rt.block_on(worker_loop(work_rx, client_tx, event_tx, config, youtube_service));
        })
}

/// Main worker loop.
async fn worker_loop(
    work_rx: Receiver<WorkRequest>,
    client_tx: Sender<ClientRequest>,
    event_tx: Sender<AppEvent>,
    config: Arc<Config>,
    youtube_service: Arc<dyn YouTubeService>,
) {
    let cli_config: CliConfig = config.as_ref().into();
    let mut orchestrator = WorkerOrchestrator::new(youtube_service);

    while let Ok(request) = work_rx.recv() {
        orchestrator
            .handle_work_request(request, client_tx.clone(), event_tx.clone(), &cli_config)
            .await;
    }
}
