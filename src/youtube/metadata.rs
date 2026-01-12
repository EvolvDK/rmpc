use crate::youtube::constants::{METADATA_REFRESH_MIN_INTERVAL, METADATA_REFRESH_THROTTLE};
use crate::youtube::db::Database;
use crate::youtube::events::YouTubeEvent;
use crate::youtube::models::{YouTubeId, YouTubeTrack};
use crate::youtube::ytdlp::YouTubeClient;
use anyhow::Context;
use async_channel::{Receiver, Sender};
use lru::LruCache;
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::time::Instant;

pub struct MetadataService<C: YouTubeClient> {
    event_tx: Sender<YouTubeEvent>,
    db: Arc<Mutex<Database>>,
    metadata_cache: Arc<RwLock<LruCache<YouTubeId, YouTubeTrack>>>,
    last_metadata_refresh: Arc<Mutex<Option<Instant>>>,
    client: Arc<C>,
    permit_tx: Sender<()>,
    permit_rx: Receiver<()>,
}

impl<C: YouTubeClient + 'static> MetadataService<C> {
    #[cfg(test)]
    pub fn new(
        event_tx: Sender<YouTubeEvent>,
        db: Arc<Mutex<Database>>,
        client: Arc<C>,
        permit_tx: Sender<()>,
        permit_rx: Receiver<()>,
    ) -> Self {
        use std::num::NonZeroUsize;
        Self {
            event_tx,
            db,
            metadata_cache: Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(100).expect("100 is non-zero"),
            ))),
            last_metadata_refresh: Arc::new(Mutex::new(None)),
            client,
            permit_tx,
            permit_rx,
        }
    }

    pub fn with_cache(
        event_tx: Sender<YouTubeEvent>,
        db: Arc<Mutex<Database>>,
        metadata_cache: Arc<RwLock<LruCache<YouTubeId, YouTubeTrack>>>,
        client: Arc<C>,
        permit_tx: Sender<()>,
        permit_rx: Receiver<()>,
    ) -> Self {
        Self {
            event_tx,
            db,
            metadata_cache,
            last_metadata_refresh: Arc::new(Mutex::new(None)),
            client,
            permit_tx,
            permit_rx,
        }
    }

    pub async fn handle_event(self: &Arc<Self>, event: YouTubeEvent) {
        match event {
            YouTubeEvent::MetadataRequest(youtube_id) => {
                let self_clone = self.clone();
                smol::spawn(async move {
                    self_clone.handle_metadata_request(YouTubeId::new(youtube_id)).await;
                })
                .detach();
            }
            YouTubeEvent::AddToLibrary(track) => {
                self.metadata_cache.write().put(track.youtube_id.clone(), track.clone());
                let db = self.db.clone();
                let event_tx = self.event_tx.clone();
                smol::unblock(move || {
                    if let Err(e) = db.lock().insert_track(&track) {
                        log::error!("Failed to add track to library: {e}");
                    } else {
                        let _ = smol::block_on(event_tx.send(YouTubeEvent::LibraryUpdated));
                    }
                })
                .detach();
            }
            YouTubeEvent::RemoveFromLibrary(youtube_id) => {
                self.metadata_cache
                    .write()
                    .pop(&crate::youtube::models::YouTubeId::new(youtube_id.clone()));
                let db = self.db.clone();
                let event_tx = self.event_tx.clone();
                smol::spawn(async move {
                    let res = smol::unblock(move || db.lock().delete_track(&youtube_id)).await;
                    if let Err(e) = res {
                        log::error!("Failed to remove track from library: {e}");
                    } else {
                        let _ = event_tx.send(YouTubeEvent::LibraryUpdated).await;
                    }
                })
                .detach();
            }
            YouTubeEvent::RemoveArtistFromLibrary(artist) => {
                let db = self.db.clone();
                let event_tx = self.event_tx.clone();
                smol::spawn(async move {
                    let res =
                        smol::unblock(move || db.lock().delete_tracks_by_artist(&artist)).await;
                    if let Err(e) = res {
                        log::error!("Failed to remove artist from library: {e}");
                    } else {
                        let _ = event_tx.send(YouTubeEvent::LibraryUpdated).await;
                    }
                })
                .detach();
            }
            YouTubeEvent::GetLibraryArtists => {
                let db = self.db.clone();
                let event_tx = self.event_tx.clone();
                smol::spawn(async move {
                    let artists = smol::unblock(move || db.lock().list_artists()).await;
                    match artists {
                        Ok(artists) => {
                            let _ = event_tx
                                .send(YouTubeEvent::LibraryArtistsAvailable(Arc::from(artists)))
                                .await;
                        }
                        Err(e) => log::error!("Failed to list artists: {e}"),
                    }
                })
                .detach();
            }
            YouTubeEvent::GetLibraryTracks(artist) => {
                let db = self.db.clone();
                let event_tx = self.event_tx.clone();
                let artist_name = artist.clone();
                smol::spawn(async move {
                    let tracks =
                        smol::unblock(move || db.lock().list_tracks_by_artist(&artist)).await;
                    match tracks {
                        Ok(tracks) => {
                            let _ = event_tx
                                .send(YouTubeEvent::LibraryTracksAvailable {
                                    artist: artist_name,
                                    tracks: Arc::from(tracks),
                                })
                                .await;
                        }
                        Err(e) => {
                            log::error!("Failed to list tracks for artist {artist_name}: {e}");
                        }
                    }
                })
                .detach();
            }
            YouTubeEvent::GetLibraryTracksAll => {
                let db = self.db.clone();
                let event_tx = self.event_tx.clone();
                smol::spawn(async move {
                    let tracks = smol::unblock(move || db.lock().list_tracks()).await;
                    match tracks {
                        Ok(tracks) => {
                            let _ = event_tx
                                .send(YouTubeEvent::AllLibraryTracksAvailable(Arc::from(tracks)))
                                .await;
                        }
                        Err(e) => log::error!("Failed to list all library tracks: {e}"),
                    }
                })
                .detach();
            }
            YouTubeEvent::MetadataRefresh => {
                let self_clone = self.clone();
                smol::spawn(async move {
                    self_clone.handle_metadata_refresh().await;
                })
                .detach();
            }
            _ => {}
        }
        smol::future::yield_now().await;
    }

    async fn handle_metadata_refresh(self: Arc<Self>) {
        {
            let mut last_refresh = self.last_metadata_refresh.lock();
            if let Some(instant) = *last_refresh {
                if instant.elapsed() < METADATA_REFRESH_MIN_INTERVAL {
                    log::debug!(
                        "Skipping metadata refresh, last refresh was less than {METADATA_REFRESH_MIN_INTERVAL:?} ago"
                    );
                    return;
                }
            }
            *last_refresh = Some(Instant::now());
        }

        log::info!("Starting metadata refresh for stale library tracks");
        let stale_threshold =
            chrono::Duration::days(crate::youtube::constants::STALE_METADATA_THRESHOLD_DAYS);
        let cutoff = chrono::Utc::now() - stale_threshold;

        let db = self.db.clone();
        let tracks = match smol::unblock(move || db.lock().list_stale_tracks(cutoff)).await {
            Ok(t) => t,
            Err(e) => {
                log::error!("Failed to list tracks for refresh: {e}");
                return;
            }
        };

        for track in tracks {
            log::debug!("Refreshing stale metadata for track: {}", track.youtube_id);
            self.fetch_and_save_metadata(track.youtube_id, Some(track.added_at)).await;

            // Wait between refreshes to avoid hammering CPU
            smol::Timer::after(METADATA_REFRESH_THROTTLE).await;
        }
        let _ = self.event_tx.send(YouTubeEvent::LibraryUpdated).await;
        log::info!("Metadata refresh completed");
    }

    async fn handle_metadata_request(self: Arc<Self>, youtube_id: YouTubeId) {
        // Check cache first
        let cached = self.metadata_cache.write().get(&youtube_id).cloned();
        if let Some(track) = cached {
            if track.is_fresh() {
                let _ = self.event_tx.send(YouTubeEvent::MetadataAvailable(Arc::new(track))).await;
                return;
            }
            // Stale but in cache, return it and refresh in background
            let _ =
                self.event_tx.send(YouTubeEvent::MetadataAvailable(Arc::new(track.clone()))).await;
            self.spawn_background_refresh(youtube_id, Some(track.added_at));
            return;
        }

        // Check DB
        let db = self.db.clone();
        let yt_id = youtube_id.clone();
        let cached_track = {
            match smol::unblock(move || db.lock().get_track(yt_id.as_str())).await {
                Ok(Some(track)) => Some(track),
                Ok(None) => None,
                Err(e) => {
                    log::error!("Database error fetching metadata: {e}");
                    None
                }
            }
        };

        if let Some(track) = cached_track {
            self.metadata_cache.write().put(youtube_id.clone(), track.clone());
            let _ =
                self.event_tx.send(YouTubeEvent::MetadataAvailable(Arc::new(track.clone()))).await;
            if !track.is_fresh() {
                self.spawn_background_refresh(youtube_id, Some(track.added_at));
            }
            return;
        }

        // Fetch from ytdlp if not in cache or DB
        self.fetch_and_save_metadata(youtube_id, None).await;
    }

    fn spawn_background_refresh(
        self: Arc<Self>,
        youtube_id: YouTubeId,
        added_at: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        smol::spawn(async move {
            self.fetch_and_save_metadata(youtube_id, added_at).await;
        })
        .detach();
    }

    async fn fetch_and_save_metadata(
        &self,
        youtube_id: YouTubeId,
        added_at: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        if let Ok(()) = self.permit_rx.recv().await {
            let res = self
                .client
                .get_metadata(youtube_id.as_str())
                .await
                .context(format!("Failed to fetch metadata for {youtube_id}"));
            match res {
                Ok(mut track) => {
                    if let Some(added) = added_at {
                        track.added_at = added;
                    }
                    self.metadata_cache.write().put(youtube_id, track.clone());
                    let db = self.db.clone();
                    let track_clone = track.clone();
                    let _ = smol::unblock(move || db.lock().insert_track(&track_clone)).await;
                    let _ =
                        self.event_tx.send(YouTubeEvent::MetadataAvailable(Arc::new(track))).await;
                }
                Err(e) => {
                    log::error!("{e:?}");
                    let _ = self.event_tx.send(YouTubeEvent::YouTubeError(e.to_string())).await;
                }
            }
            let _ = self.permit_tx.send(()).await;
        }
    }
}
