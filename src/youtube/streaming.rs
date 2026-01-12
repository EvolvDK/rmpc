use crate::youtube::events::YouTubeEvent;
use crate::youtube::models::{YouTubeId, YouTubeTrack};
use crate::youtube::ytdlp::YouTubeClient;
use anyhow::Context;
use async_channel::{Receiver, Sender};
use lru::LruCache;
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

type PendingRequest = Vec<Sender<Option<String>>>;

pub struct StreamingService<C: YouTubeClient> {
    event_tx: Sender<YouTubeEvent>,
    url_cache: Arc<RwLock<crate::youtube::cache::ExpiringCache<YouTubeId, String>>>,
    metadata_cache: Arc<RwLock<LruCache<YouTubeId, crate::youtube::models::YouTubeTrack>>>,
    client: Arc<C>,
    pending_refreshes: Arc<RwLock<HashSet<YouTubeId>>>,
    pending_resolutions: Arc<Mutex<HashMap<YouTubeId, PendingRequest>>>,
}

impl<C: YouTubeClient + 'static> StreamingService<C> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_tx: Sender<YouTubeEvent>,
        client: Arc<C>,
        url_cache: Arc<RwLock<crate::youtube::cache::ExpiringCache<YouTubeId, String>>>,
        metadata_cache: Arc<RwLock<LruCache<YouTubeId, YouTubeTrack>>>,
        _permit_tx: Sender<()>,
        _permit_rx: Receiver<()>,
    ) -> Self {
        Self {
            event_tx,
            url_cache,
            metadata_cache,
            client,
            pending_refreshes: Arc::new(RwLock::new(HashSet::new())),
            pending_resolutions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn handle_event(self: &Arc<Self>, event: YouTubeEvent) {
        let inner = self.clone();
        match event {
            YouTubeEvent::AddToQueue(track) => {
                smol::spawn(async move {
                    inner.handle_add_to_queue(track).await;
                })
                .detach();
            }
            YouTubeEvent::AddManyToQueue(tracks) => {
                smol::spawn(async move {
                    inner.handle_add_many_to_queue(tracks).await;
                })
                .detach();
            }
            YouTubeEvent::ReplaceQueue(tracks) => {
                smol::spawn(async move {
                    inner.handle_replace_queue(tracks).await;
                })
                .detach();
            }
            YouTubeEvent::ClearQueue => {
                let _ = self.event_tx.send(YouTubeEvent::ClearQueue).await;
            }
            YouTubeEvent::AddToPlaylist { playlist, tracks } => {
                smol::spawn(async move {
                    inner.handle_add_to_playlist(playlist, tracks).await;
                })
                .detach();
            }
            YouTubeEvent::CreatePlaylist { name, tracks } => {
                smol::spawn(async move {
                    inner.handle_create_playlist(name, tracks).await;
                })
                .detach();
            }
            YouTubeEvent::PlaybackError { youtube_id, pos, play_after_refresh, .. } => {
                smol::spawn(async move {
                    inner.handle_playback_error(youtube_id, pos, play_after_refresh).await;
                })
                .detach();
            }
            YouTubeEvent::RefreshRequest { youtube_id, pos, play_after_refresh } => {
                smol::spawn(async move {
                    inner.handle_refresh_request(youtube_id, pos, play_after_refresh).await;
                })
                .detach();
            }
            _ => {}
        }
    }

    async fn handle_add_many_to_queue(&self, tracks: Arc<[crate::youtube::models::YouTubeTrack]>) {
        let mut added = 0;
        let mut skipped = 0;
        for track in tracks.iter() {
            if self.handle_add_to_queue(track.clone()).await {
                added += 1;
            } else {
                skipped += 1;
            }
        }
        let _ = self.event_tx.send(YouTubeEvent::AddManyToQueueResult { added, skipped }).await;
    }

    async fn handle_replace_queue(&self, tracks: Arc<[crate::youtube::models::YouTubeTrack]>) {
        let _ = self.event_tx.send(YouTubeEvent::ClearQueue).await;
        for track in tracks.iter() {
            self.handle_add_to_queue(track.clone()).await;
        }
    }

    async fn handle_playback_error(
        &self,
        youtube_id: String,
        pos: Option<u32>,
        play_after_refresh: bool,
    ) {
        log::warn!("Recovering from playback error for {youtube_id}");
        self.handle_refresh(youtube_id, pos, play_after_refresh).await;
    }

    async fn handle_refresh_request(
        &self,
        youtube_id: String,
        pos: Option<u32>,
        play_after_refresh: bool,
    ) {
        log::info!("Refreshing expired YouTube URL for {youtube_id}");
        self.handle_refresh(youtube_id, pos, play_after_refresh).await;
    }

    async fn handle_refresh(&self, youtube_id: String, pos: Option<u32>, play_after_refresh: bool) {
        let yt_id = YouTubeId::new(youtube_id);

        {
            let mut pending = self.pending_refreshes.write();
            if pending.contains(&yt_id) {
                log::debug!("Refresh already in progress for {yt_id}, skipping duplicate request");
                return;
            }
            pending.insert(yt_id.clone());
        }

        self.url_cache.write().pop(&yt_id);
        match self.get_streaming_url(&yt_id).await {
            Ok(url) => {
                let streaming_url = yt_id.append_to_url(&url);
                let _ = self
                    .event_tx
                    .send(YouTubeEvent::QueueUrlReplace {
                        url: streaming_url,
                        youtube_id: yt_id.to_string(),
                        pos,
                        play_after_replace: play_after_refresh,
                    })
                    .await;
            }
            Err(e) => {
                log::error!("{e:?}");
            }
        }

        self.pending_refreshes.write().remove(&yt_id);
    }

    async fn resolve_tracks_to_urls(&self, tracks: &[YouTubeTrack]) -> Vec<String> {
        let mut urls = Vec::with_capacity(tracks.len());
        for track in tracks {
            match self.get_streaming_url(&track.youtube_id).await {
                Ok(url) => {
                    urls.push(track.youtube_id.append_to_url(&url));
                }
                Err(e) => {
                    log::error!("{e:?}");
                }
            }
        }
        urls
    }

    async fn handle_add_to_queue(&self, track: crate::youtube::models::YouTubeTrack) -> bool {
        let youtube_id = track.youtube_id.clone();

        match self.get_streaming_url(&youtube_id).await {
            Ok(url) => {
                // Cache metadata so it's available for enrichment immediately after adding to MPD queue
                self.metadata_cache.write().put(youtube_id.clone(), track.clone());

                // Append fragment for identification in MPD
                let streaming_url = track.youtube_id.append_to_url(&url);
                let _ = self.event_tx.send(YouTubeEvent::QueueUrl(streaming_url.clone())).await;
                let _ = self.event_tx.send(YouTubeEvent::MetadataAvailable(Arc::new(track))).await;
                log::info!("Ready to stream: {streaming_url}");
                true
            }
            Err(e) => {
                log::error!("{e:?}");
                let _ = self
                    .event_tx
                    .send(YouTubeEvent::PlaybackError {
                        youtube_id: youtube_id.to_string(),
                        pos: None,
                        _message: e.to_string(),
                        play_after_refresh: false,
                    })
                    .await;
                false
            }
        }
    }

    async fn handle_add_to_playlist(
        &self,
        playlist: String,
        tracks: Arc<[crate::youtube::models::YouTubeTrack]>,
    ) {
        let urls = self.resolve_tracks_to_urls(&tracks).await;

        if urls.is_empty() {
            let _ = self
                .event_tx
                .send(YouTubeEvent::YouTubeError(
                    "Failed to resolve any streaming URLs".to_string(),
                ))
                .await;
        } else {
            let _ = self
                .event_tx
                .send(YouTubeEvent::AddToPlaylistResult { playlist, urls: Arc::from(urls) })
                .await;
        }
    }

    async fn handle_create_playlist(
        &self,
        name: String,
        tracks: Arc<[crate::youtube::models::YouTubeTrack]>,
    ) {
        let urls = self.resolve_tracks_to_urls(&tracks).await;

        if urls.is_empty() {
            let _ = self
                .event_tx
                .send(YouTubeEvent::YouTubeError(
                    "Failed to resolve any streaming URLs".to_string(),
                ))
                .await;
        } else {
            let _ = self
                .event_tx
                .send(YouTubeEvent::CreatePlaylistResult { name, urls: Arc::from(urls) })
                .await;
        }
    }

    async fn get_streaming_url(
        &self,
        youtube_id: &YouTubeId,
    ) -> crate::youtube::error::Result<String> {
        // Phase 1: Check cache (fast path, no yt-dlp call)
        if let Some(url) = self.url_cache.write().get(youtube_id) {
            log::debug!("Cache hit for {youtube_id}");
            return Ok(url.clone());
        }

        // Phase 2: Attempt to become the "leader" for this YouTube ID
        let rx = {
            let mut pending = self.pending_resolutions.lock();

            // Check-and-insert happens atomically under single lock
            if let Some(waiters) = pending.get_mut(youtube_id) {
                // Another task is already resolving this ID - become a follower
                log::debug!("Resolution in progress for {youtube_id}, waiting...");
                let (tx, rx) = async_channel::bounded(1);
                waiters.push(tx);
                Some(rx)
            } else {
                // We're the leader - insert empty waiter list
                pending.insert(youtube_id.clone(), Vec::new());
                None
            }
        };

        // Phase 3a: If follower, wait for leader's result
        if let Some(rx) = rx {
            return rx
                .recv()
                .await
                .map_err(|e| crate::youtube::error::YouTubeError::Anyhow(anyhow::anyhow!(e)))?
                .context(format!("Leader failed to resolve {youtube_id}"))
                .map_err(crate::youtube::error::YouTubeError::from);
        }

        // Phase 3b: Leader path - call yt-dlp
        log::debug!("Calling yt-dlp for {youtube_id} (leader)");
        let result = self
            .client
            .get_streaming_url(youtube_id.as_str())
            .await
            .context(format!("Failed to resolve streaming URL for {youtube_id}"));

        // Phase 4: Broadcast result to all waiters and cleanup
        let waiters = {
            let mut pending = self.pending_resolutions.lock();

            // Cache successful results
            if let Ok(url) = &result {
                log::debug!("Successfully resolved {youtube_id}");
                self.url_cache.write().put(youtube_id.clone(), url.clone());
            }

            // Notify all followers (if any)
            pending.remove(youtube_id)
        };

        if let Some(waiters) = waiters {
            if !waiters.is_empty() {
                log::debug!("Notifying {} waiters for {}", waiters.len(), youtube_id);
            }
            for tx in waiters {
                // Best-effort notification (ignore send errors if waiter cancelled)
                let _ = tx.send(result.as_ref().map(|s| s.clone()).ok()).await;
            }
        }

        result.map_err(crate::youtube::error::YouTubeError::from)
    }
}
