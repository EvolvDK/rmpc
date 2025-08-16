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
    youtube,
};

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

async fn worker_loop(
    work_rx: Receiver<WorkRequest>,
    client_tx: Sender<ClientRequest>,
    event_tx: Sender<AppEvent>,
    config: Arc<Config>,
) {
    let cli_config: CliConfig = config.as_ref().into();
    let youtube_cache_ttl = config.youtube_cache_ttl;
    let mut youtube_search_handle: Option<JoinHandle<()>> = None;

    while let Ok(req) = work_rx.recv() {
        if let WorkRequest::YouTubeSearch { query, generation } = req {
            // Annuler la recherche précédente si elle existe
            if let Some(handle) = youtube_search_handle.take() {
                handle.abort();
            }

            let event_tx_clone = event_tx.clone();
            let new_handle = tokio::spawn(async move {
                if let Err(err) =
                    youtube::search(&query, generation, youtube_cache_ttl, event_tx_clone).await
                {
                    log::error!("YouTube search failed: {}", err);
                }
            });
            youtube_search_handle = Some(new_handle);
        } else {
            // Gérer les autres requêtes de manière non bloquante
            let client_tx_clone = client_tx.clone();
            let event_tx_clone = event_tx.clone();
            let cli_config_clone = cli_config.clone();

            tokio::spawn(async move {
                if let Err(err) = handle_work_request(
                    req,
                    &client_tx_clone,
                    &event_tx_clone,
                    &cli_config_clone,
                    youtube_cache_ttl,
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

async fn handle_work_request(
    request: WorkRequest,
    client_tx: &Sender<ClientRequest>,
    event_tx: &Sender<AppEvent>,
    config: &CliConfig,
    youtube_cache_ttl: Duration,
) -> Result<()> {
    match request {
        WorkRequest::Command(command) => {
            let callback = command.execute(config)?;
            try_skip!(
                client_tx.send(ClientRequest::Command(MpdCommand { callback })),
                "Failed to send client request to complete command"
            );
            event_tx.send(AppEvent::WorkDone(Ok(WorkDone::None)))?;
        }
        WorkRequest::IndexLyrics { lyrics_dir } => {
            let index = LrcIndex::index(&PathBuf::from(lyrics_dir));
            event_tx.send(AppEvent::WorkDone(Ok(WorkDone::LyricsIndexed { index })))?;
        }
        WorkRequest::IndexSingleLrc { path } => {
            let result = WorkDone::SingleLrcIndexed { lrc_entry: LrcIndex::index_single(path)? };
            event_tx.send(AppEvent::WorkDone(Ok(result)))?;
        }
        WorkRequest::GetYouTubeStreamUrl { video, context } => {
            let result = youtube::get_stream_url(&video.youtube_id, youtube_cache_ttl).await;
            let work_done = match result {
                Ok(url) => WorkDone::YouTubeStreamUrlReady { url, video, context },
                Err(_) => WorkDone::YouTubeStreamUrlFailed { video, context },
            };
            event_tx.send(AppEvent::WorkDone(Ok(work_done)))?;
        }
        WorkRequest::RefreshYouTubeStream {
            old_song_id,
            position,
            youtube_id,
            video_title,
        } => {
            let result = youtube::get_stream_url_and_metadata(&youtube_id).await;
            let work_done = match result {
                Ok((new_url, video)) => WorkDone::YouTubeStreamRefreshed {
                    new_url,
                    video,
                    old_song_id,
                    position,
                },
                Err(_) => WorkDone::YouTubeStreamRefreshFailed { video_title },
            };
            event_tx.send(AppEvent::WorkDone(Ok(work_done)))?;
        }
        WorkRequest::YouTubeGetVideoInfo { id } => {
            if let Ok(Some(video)) = youtube::get_video_info(&id, youtube_cache_ttl).await {
                event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeVideoInfoFetched(
                    video,
                ))))?;
            }
        }
        // Ce cas est maintenant géré dans la boucle principale
        WorkRequest::YouTubeSearch { .. } => unreachable!(),
    }
    Ok(())
}
