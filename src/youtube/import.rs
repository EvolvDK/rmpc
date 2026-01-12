use crate::config::{MpdAddress, address::MpdPassword};
use crate::mpd::client::Client;
use crate::mpd::mpd_client::MpdClient;
use crate::shared::macros::status_info;
use crate::youtube::constants::DATABASE_BATCH_SIZE;
use crate::youtube::db::Database;
use crate::youtube::error::YouTubeError;
use crate::youtube::events::YouTubeEvent;
use crate::youtube::models::{YouTubeId, YouTubeTrack};
use crate::youtube::ytdlp::YtdlpAdapter;
use anyhow::{Context, Result};
use async_channel::{Receiver, Sender};
use parking_lot::Mutex;
use std::sync::Arc;

pub struct ImportService {
    event_tx: Sender<YouTubeEvent>,
    db: Arc<Mutex<Database>>,
    address: MpdAddress,
    password: Option<MpdPassword>,
    permit_tx: Sender<()>,
    permit_rx: Receiver<()>,
}

impl ImportService {
    pub fn new(
        event_tx: Sender<YouTubeEvent>,
        db: Arc<Mutex<Database>>,
        address: MpdAddress,
        password: Option<MpdPassword>,
        permit_tx: Sender<()>,
        permit_rx: Receiver<()>,
    ) -> Self {
        Self { event_tx, db, address, password, permit_tx, permit_rx }
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
        let mut success = 0;
        let mut failed = 0;

        let res = smol::unblock(move || -> Result<Vec<YouTubeId>> {
            let mut rdr = csv::Reader::from_path(path_obj)
                .context("Failed to open library import CSV file")?;
            let mut ids = Vec::new();
            for result in rdr.records() {
                let record = result.context("Failed to read CSV record")?;
                if let Some(url_or_id) = record.get(0) {
                    if let Some(id) = YouTubeId::from_any(url_or_id) {
                        ids.push(id);
                    }
                }
            }
            Ok(ids)
        })
        .await;

        let ids = match res {
            Ok(ids) => ids,
            Err(e) => {
                self.handle_import_error(e, "failed to parse library import file").await;
                return;
            }
        };

        let total = ids.len();
        if total == 0 {
            let _ =
                self.event_tx.send(YouTubeEvent::ImportFinished { success: 0, failed: 0 }).await;
            return;
        }

        let (task_tx, task_rx) = async_channel::bounded::<YouTubeId>(5);
        let (res_tx, res_rx) =
            async_channel::bounded::<Result<Option<YouTubeTrack>, (YouTubeId, YouTubeError)>>(100);

        // Spawn workers
        for _ in 0..5 {
            let task_rx = task_rx.clone();
            let res_tx = res_tx.clone();
            let inner = Arc::new(self.clone_for_workers());
            smol::spawn(async move {
                while let Ok(id) = task_rx.recv().await {
                    match inner.fetch_track_metadata(id.clone()).await {
                        Ok(track) => {
                            let _ = res_tx.send(Ok(track)).await;
                        }
                        Err(e) => {
                            let _ = res_tx.send(Err((id, e))).await;
                        }
                    }
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
        let mut processed = 0;
        let mut reporter =
            ProgressReporter::new(self.event_tx.clone(), total, "Importing".to_string());

        while processed < total {
            match res_rx.recv().await {
                Ok(Ok(Some(track))) => {
                    tracks_to_insert.push(track);
                    if tracks_to_insert.len() >= DATABASE_BATCH_SIZE {
                        let db = self.db.clone();
                        let tracks = std::mem::take(&mut tracks_to_insert);
                        let count = tracks.len();
                        if let Err(e) =
                            smol::unblock(move || db.lock().insert_tracks_batch(&tracks)).await
                        {
                            log::error!("Failed to batch insert tracks: {e}");
                            failed += count;
                        } else {
                            success += count;
                        }
                    }
                }
                Ok(Ok(None)) => success += 1,
                Ok(Err((id, e))) => {
                    log::error!("Failed to fetch metadata for imported track {id}: {e}");
                    failed += 1;
                }
                Err(e) => {
                    log::error!("Import result channel closed unexpectedly: {e}");
                    break;
                }
            }

            processed += 1;
            reporter.report(processed).await;
        }

        if !tracks_to_insert.is_empty() {
            let db = self.db.clone();
            let tracks = tracks_to_insert;
            let len = tracks.len();
            if let Err(e) = smol::unblock(move || db.lock().insert_tracks_batch(&tracks)).await {
                log::error!("Failed to batch insert remaining tracks: {e}");
                failed += len;
            } else {
                success += len;
            }
        }

        let _ = self.event_tx.send(YouTubeEvent::LibraryUpdated).await;
        let _ = self.event_tx.send(YouTubeEvent::ImportFinished { success, failed }).await;
        log::info!("Library import finished: {success} success, {failed} failed");
    }

    async fn handle_import_playlists(&self, path: String) {
        log::info!("Starting playlist import from folder {path}");
        let path = std::path::Path::new(&path);
        if !path.is_dir() {
            self.handle_import_error(anyhow::anyhow!("Path is not a directory"), "playlist import")
                .await;
            return;
        }

        // Recursively find CSV and JSON files
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) => {
                self.handle_import_error(e.into(), "failed to read directory").await;
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                if extension == "csv" || extension == "json" {
                    let playlist_name =
                        path.file_stem().and_then(|s| s.to_str()).unwrap_or("Imported Playlist");
                    log::info!("Importing playlist: {playlist_name}");

                    // Extract IDs (simplified logic for now)
                    let ids = if extension == "csv" {
                        let p = path.clone();
                        smol::unblock(move || parse_csv_for_ids(&p)).await
                    } else {
                        Vec::new() // JSON parsing not yet implemented
                    };

                    if !ids.is_empty() {
                        let total = ids.len();
                        let mut streaming_urls = Vec::with_capacity(total);
                        let mut reporter = ProgressReporter::new(
                            self.event_tx.clone(),
                            total,
                            format!("Importing {playlist_name} ({extension})"),
                        );

                        let mut tracks_to_insert = Vec::new();
                        for (i, id) in ids.iter().enumerate() {
                            let current = i + 1;
                            reporter.report(current).await;

                            // Fetch metadata to get link and cache it
                            match self.fetch_track_metadata(id.clone()).await {
                                Ok(Some(track)) => {
                                    streaming_urls
                                        .push(track.youtube_id.append_to_url(&track.link));
                                    tracks_to_insert.push(track);

                                    if tracks_to_insert.len() >= DATABASE_BATCH_SIZE {
                                        let db = self.db.clone();
                                        let tracks = std::mem::take(&mut tracks_to_insert);
                                        let _ = smol::unblock(move || {
                                            db.lock().insert_tracks_batch(&tracks)
                                        })
                                        .await;
                                    }
                                }
                                Ok(None) => {
                                    // Track already in DB, we still need its link for the playlist
                                    let id_str = id.to_string();
                                    let db = self.db.clone();
                                    if let Ok(Some(track)) =
                                        smol::unblock(move || db.lock().get_track(&id_str)).await
                                    {
                                        streaming_urls
                                            .push(track.youtube_id.append_to_url(&track.link));
                                    }
                                }
                                Err(e) => {
                                    log::error!(
                                        "Failed to get metadata for {id} in playlist {playlist_name}: {e}"
                                    );
                                }
                            }
                        }

                        if !tracks_to_insert.is_empty() {
                            let db = self.db.clone();
                            let _ = smol::unblock(move || {
                                db.lock().insert_tracks_batch(&tracks_to_insert)
                            })
                            .await;
                        }

                        if !streaming_urls.is_empty() {
                            // Use MPD client to save as a playlist
                            let addr = self.address.clone();
                            let password = self.password.clone();
                            let pl_name = playlist_name.to_owned();
                            let res = smol::unblock(move || {
                                let mut client =
                                    Client::init(addr, password, "import", None, false)?;
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
                    }
                }
            }
        }

        let _ = self.event_tx.send(YouTubeEvent::LibraryUpdated).await;
        status_info!("Playlist import from {} finished", path.display());
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
            let res = YtdlpAdapter::get_metadata(id.as_str()).await;
            let _ = self.permit_tx.send(()).await;
            res.map(Some)
        } else {
            Err(YouTubeError::Anyhow(anyhow::anyhow!("Import permit channel closed")))
        }
    }

    async fn handle_import_error(&self, error: anyhow::Error, context: &str) {
        log::error!("Import failed ({context}): {error}");
        let _ = self.event_tx.send(YouTubeEvent::ImportFinished { success: 0, failed: 1 }).await;
    }

    fn clone_for_workers(&self) -> Self {
        Self {
            event_tx: self.event_tx.clone(),
            db: self.db.clone(),
            address: self.address.clone(),
            password: self.password.clone(),
            permit_tx: self.permit_tx.clone(),
            permit_rx: self.permit_rx.clone(),
        }
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
            let _ = self
                .event_tx
                .send(YouTubeEvent::ImportProgress {
                    current: self.processed,
                    total: self.total,
                    message: format!("{} {}/{}", self.message_prefix, self.processed, self.total),
                })
                .await;
            self.last_update = std::time::Instant::now();
        }
    }
}

fn parse_csv_for_ids(path: &std::path::Path) -> Vec<YouTubeId> {
    let mut ids = Vec::new();
    if let Ok(mut rdr) = csv::Reader::from_path(path) {
        for result in rdr.records().flatten() {
            if let Some(url_or_id) = result.get(0) {
                if let Some(id) = YouTubeId::from_any(url_or_id) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}
