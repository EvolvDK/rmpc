use crate::config::{MpdAddress, address::MpdPassword};
use crate::mpd::client::Client;
use crate::mpd::mpd_client::MpdClient;
use crate::youtube::constants::DATABASE_BATCH_SIZE;
use crate::youtube::db::Database;
use crate::youtube::error::YouTubeError;
use crate::youtube::events::YouTubeEvent;
use crate::youtube::models::{YouTubeId, YouTubeTrack};
use crate::youtube::utils::parse_csv_for_ids;
use crate::youtube::ytdlp::YouTubeClient;
use anyhow::Result;
use async_channel::{Receiver, Sender};
use parking_lot::Mutex;
use std::sync::Arc;

pub struct ImportService<C: YouTubeClient> {
    event_tx: Sender<YouTubeEvent>,
    db: Arc<Mutex<Database>>,
    address: MpdAddress,
    password: Option<MpdPassword>,
    permit_tx: Sender<()>,
    permit_rx: Receiver<()>,
    client: Arc<C>,
}

impl<C: YouTubeClient> Clone for ImportService<C> {
    fn clone(&self) -> Self {
        Self {
            event_tx: self.event_tx.clone(),
            db: self.db.clone(),
            address: self.address.clone(),
            password: self.password.clone(),
            permit_tx: self.permit_tx.clone(),
            permit_rx: self.permit_rx.clone(),
            client: self.client.clone(),
        }
    }
}

impl<C: YouTubeClient + 'static> ImportService<C> {
    pub fn new(
        event_tx: Sender<YouTubeEvent>,
        db: Arc<Mutex<Database>>,
        address: MpdAddress,
        password: Option<MpdPassword>,
        permit_tx: Sender<()>,
        permit_rx: Receiver<()>,
        client: Arc<C>,
    ) -> Self {
        Self { event_tx, db, address, password, permit_tx, permit_rx, client }
    }

    pub async fn handle_event(self: &Arc<Self>, event: YouTubeEvent) {
        let inner = self.clone();
        match event {
            YouTubeEvent::ImportLibrary(path) => {
                smol::spawn(async move {
                    inner.handle_import_library(path).await;
                })
                .detach();
            }
            YouTubeEvent::ImportPlaylists(path) => {
                smol::spawn(async move {
                    inner.handle_import_playlists(path).await;
                })
                .detach();
            }
            _ => {}
        }
        smol::future::yield_now().await;
    }

    async fn handle_import_library(&self, path: String) {
        log::info!("Starting library import from {path}");
        let path_obj = std::path::PathBuf::from(&path);

        let ids = match smol::unblock(move || parse_csv_for_ids(&path_obj)).await {
            ids if !ids.is_empty() => ids,
            _ => {
                let _ = self
                    .event_tx
                    .send(YouTubeEvent::ImportFinished { success: 0, skipped: 0, failed: 0 })
                    .await;
                return;
            }
        };

        let (added, skipped, failed, _) =
            self.process_ids(ids, "Importing".to_string(), false).await;

        let _ = self.event_tx.send(YouTubeEvent::LibraryUpdated).await;
        let _ = self
            .event_tx
            .send(YouTubeEvent::ImportFinished { success: added, skipped, failed })
            .await;
    }

    async fn handle_import_playlists(&self, path: String) {
        log::info!("Starting playlist import from folder {path}");
        let path_obj = std::path::Path::new(&path);
        if !path_obj.is_dir() {
            self.handle_import_error(anyhow::anyhow!("Path is not a directory"), "playlist import")
                .await;
            return;
        }

        let entries = match std::fs::read_dir(path_obj) {
            Ok(entries) => entries,
            Err(e) => {
                self.handle_import_error(e.into(), "failed to read directory").await;
                return;
            }
        };

        let (mut total_added, mut total_skipped) = (0, 0);
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Ok((added, skipped)) = self.import_playlist_file(&path).await {
                    total_added += added;
                    total_skipped += skipped;
                }
            }
        }

        let _ = self.event_tx.send(YouTubeEvent::LibraryUpdated).await;
        let _ = self
            .event_tx
            .send(YouTubeEvent::ImportFinished {
                success: total_added,
                skipped: total_skipped,
                failed: 0,
            })
            .await;
    }

    async fn import_playlist_file(&self, path: &std::path::Path) -> Result<(usize, usize)> {
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if extension != "csv" && extension != "json" {
            return Ok((0, 0));
        }

        let playlist_name =
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("Imported Playlist").to_string();
        log::info!("Importing playlist: {playlist_name}");

        let ids = if extension == "csv" {
            let p = path.to_path_buf();
            smol::unblock(move || parse_csv_for_ids(&p)).await
        } else {
            Vec::new() // JSON parsing not yet implemented
        };

        if ids.is_empty() {
            Ok((0, 0))
        } else {
            let prefix = format!("Importing {playlist_name} ({extension})");
            let (added, skipped, _, tracks) = self.process_ids(ids, prefix, true).await;

            if !tracks.is_empty() {
                let streaming_urls =
                    tracks.into_iter().map(|t| t.youtube_id.append_to_url(&t.link)).collect();
                self.save_mpd_playlist(&playlist_name, streaming_urls).await;
            }
            Ok((added, skipped))
        }
    }

    async fn process_ids(
        &self,
        ids: Vec<YouTubeId>,
        message_prefix: String,
        return_tracks: bool,
    ) -> (usize, usize, usize, Vec<YouTubeTrack>) {
        let total = ids.len();
        if total == 0 {
            return (0, 0, 0, Vec::new());
        }

        let (task_tx, task_rx) = async_channel::bounded::<YouTubeId>(5);
        let (res_tx, res_rx) =
            async_channel::bounded::<(YouTubeId, Result<Option<YouTubeTrack>, YouTubeError>)>(100);

        // Spawn workers
        for _ in 0..5 {
            let task_rx = task_rx.clone();
            let res_tx = res_tx.clone();
            let inner = self.clone();
            smol::spawn(async move {
                while let Ok(id) = task_rx.recv().await {
                    let result = inner.fetch_track_metadata(id.clone()).await;
                    let _ = res_tx.send((id, result)).await;
                }
            })
            .detach();
        }

        // Feed tasks
        smol::spawn(async move {
            for id in ids {
                if let Err(e) = task_tx.send(id).await {
                    log::error!("Failed to send import task: {e}");
                    break;
                }
            }
        })
        .detach();

        let mut tracks_to_insert = Vec::new();
        let mut result_tracks = Vec::new();
        let mut added = 0;
        let mut skipped = 0;
        let mut failed = 0;
        let mut processed = 0;
        let mut reporter = ProgressReporter::new(self.event_tx.clone(), total, message_prefix);

        while processed < total {
            match res_rx.recv().await {
                Ok((id, result)) => {
                    match result {
                        Ok(Some(track)) => {
                            if return_tracks {
                                result_tracks.push(track.clone());
                            }
                            tracks_to_insert.push(track);
                            if tracks_to_insert.len() >= DATABASE_BATCH_SIZE {
                                match self.flush_batch(&mut tracks_to_insert).await {
                                    Ok(n) => added += n,
                                    Err(_) => failed += DATABASE_BATCH_SIZE,
                                }
                            }
                        }
                        Ok(None) => {
                            skipped += 1;
                            if return_tracks {
                                // Fetch from DB to get details
                                let db = self.db.clone();
                                let id_str = id.to_string();
                                if let Ok(Some(track)) =
                                    smol::unblock(move || db.lock().get_track(&id_str)).await
                                {
                                    result_tracks.push(track);
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to fetch metadata for imported track {id}: {e}");
                            failed += 1;
                        }
                    }
                }
                Err(e) => {
                    log::error!("Import result channel closed unexpectedly: {e}");
                    break;
                }
            }

            processed += 1;
            reporter.report(processed).await;
        }

        // Flush remaining
        if !tracks_to_insert.is_empty() {
            match self.flush_batch(&mut tracks_to_insert).await {
                Ok(n) => added += n,
                Err(n) => failed += n,
            }
        }

        (added, skipped, failed, result_tracks)
    }

    async fn flush_batch(&self, tracks: &mut Vec<YouTubeTrack>) -> Result<usize, usize> {
        if tracks.is_empty() {
            return Ok(0);
        }
        let len = tracks.len();
        let db = self.db.clone();
        let batch = std::mem::take(tracks);
        if let Err(e) = smol::unblock(move || db.lock().insert_tracks_batch(&batch)).await {
            log::error!("Failed to batch insert tracks: {e}");
            Err(len)
        } else {
            Ok(len)
        }
    }

    async fn save_mpd_playlist(&self, playlist_name: &str, streaming_urls: Vec<String>) {
        let addr = self.address.clone();
        let password = self.password.clone();
        let pl_name = playlist_name.to_owned();
        let res = smol::unblock(move || {
            let mut client = Client::init(addr, password, "import", None, false)?;
            for url in streaming_urls {
                client.add_to_playlist(&pl_name, &url, None)?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await;

        if let Err(e) = res {
            log::error!("Failed to save MPD playlist {playlist_name}: {e}");
        } else {
            log::info!("Successfully imported playlist {playlist_name} to MPD");
        }
    }

    async fn fetch_track_metadata(
        &self,
        id: YouTubeId,
    ) -> Result<Option<YouTubeTrack>, YouTubeError> {
        let db = self.db.clone();
        let id_str = id.to_string();
        let exists = smol::unblock(move || {
            db.lock().get_track(&id_str).map(|t| t.is_some()).unwrap_or(false)
        })
        .await;

        if exists {
            return Ok(None);
        }

        if let Ok(()) = self.permit_rx.recv().await {
            let res = self.client.get_metadata(id.as_str()).await;
            let _ = self.permit_tx.send(()).await;
            res.map(Some)
        } else {
            Err(YouTubeError::Anyhow(anyhow::anyhow!("Import permit channel closed")))
        }
    }

    async fn handle_import_error(&self, error: anyhow::Error, context: &str) {
        let msg = format!("Import failed ({context}): {error}");
        let _ = self.event_tx.send(YouTubeEvent::YouTubeError(msg)).await;
        let _ = self
            .event_tx
            .send(YouTubeEvent::ImportFinished { success: 0, skipped: 0, failed: 1 })
            .await;
    }
}

struct ProgressReporter {
    event_tx: Sender<YouTubeEvent>,
    total: usize,
    processed: usize,
    last_update: std::time::Instant,
    message_prefix: String,
}

impl ProgressReporter {
    fn new(event_tx: Sender<YouTubeEvent>, total: usize, message_prefix: String) -> Self {
        Self {
            event_tx,
            total,
            processed: 0,
            last_update: std::time::Instant::now(),
            message_prefix,
        }
    }

    async fn report(&mut self, current: usize) {
        self.processed = current;
        if self.processed == 1
            || self.processed == self.total
            || self.processed % 10 == 0
            || self.last_update.elapsed() > std::time::Duration::from_millis(200)
        {
            let message = format!("{} {}/{}", self.message_prefix, self.processed, self.total);
            let _ = self
                .event_tx
                .send(YouTubeEvent::ImportProgress {
                    current: self.processed,
                    total: self.total,
                    message,
                })
                .await;
            self.last_update = std::time::Instant::now();
        }
    }
}
