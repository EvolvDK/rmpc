use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use crossbeam::channel::{Receiver, Sender};

use crate::{
    config::{Config, cli_config::CliConfig},
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
    std::thread::Builder::new().name("work".to_owned()).spawn(move || {
        let cli_config = config.as_ref().into();
        let youtube_cache_ttl = config.youtube_cache_ttl;
        while let Ok(req) = work_rx.recv() {
            if let Err(err) =
                handle_work_request(req, &client_tx, &event_tx, &cli_config, youtube_cache_ttl)
            {
                try_skip!(
                    event_tx.send(AppEvent::WorkDone(Err(err))),
                    "Failed to send work done error notification"
                );
            }
        }
    })
}

fn handle_work_request(
    request: WorkRequest,
    client_tx: &Sender<ClientRequest>,
    event_tx: &Sender<AppEvent>,
    config: &CliConfig,
    youtube_cache_ttl: Duration,
) -> Result<()> {
    match request {
        WorkRequest::Command(command) => {
            let callback = command.execute(config)?; // TODO log
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
        WorkRequest::YouTubeSearch { query } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            rt.block_on(youtube::search(&query, youtube_cache_ttl, event_tx.clone()))?;
        }
        WorkRequest::GetYouTubeStreamUrl { video, context } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let result = rt.block_on(youtube::get_stream_url(&video.youtube_id, youtube_cache_ttl));
            let work_done = match result {
                Ok(url) => WorkDone::YouTubeStreamUrlReady { url, video, context },
                Err(_) => WorkDone::YouTubeStreamUrlFailed { video, context },
            };
            event_tx.send(AppEvent::WorkDone(Ok(work_done)))?;
        }
        WorkRequest::YouTubeGetVideoInfo { id } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            if let Ok(Some(video)) = rt.block_on(youtube::get_video_info(&id, youtube_cache_ttl)) {
                event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeVideoInfoFetched(
                    video,
                ))))?;
            }
        }
    }
    Ok(())
}
