use crate::youtube::cache::ExpiringCache;
use crate::youtube::constants::{
    CONCURRENT_YTDLP_LIMIT, METADATA_CACHE_SIZE, METADATA_REFRESH_INTERVAL, STARTUP_DELAY,
    URL_CACHE_SIZE,
};
use crate::youtube::db::Database;
use crate::youtube::error::Result;
use crate::youtube::events::YouTubeEvent;
use crate::youtube::metadata::MetadataService;
use crate::youtube::models::{YouTubeId, YouTubeTrack};
use crate::youtube::search::SearchService;
use crate::youtube::streaming::StreamingService;
use crate::youtube::ytdlp::YtdlpAdapter;

use crate::mpd::commands::Song;

use crate::config::{MpdAddress, address::MpdPassword};
use async_channel::{Receiver, Sender, bounded};
use lru::LruCache;
use parking_lot::{Mutex, RwLock};
use std::fmt::Formatter;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

pub struct YouTubeManager {
    request_tx: Sender<YouTubeEvent>,
    request_rx: Receiver<YouTubeEvent>,
    event_tx: Sender<YouTubeEvent>,
    event_rx: Receiver<YouTubeEvent>,
    pub db: Arc<Mutex<Database>>,
    metadata_cache: Arc<RwLock<LruCache<YouTubeId, YouTubeTrack>>>,
    tasks: Arc<Mutex<Vec<smol::Task<()>>>>,

    // Services for direct addressing
    search: Arc<SearchService<YtdlpAdapter>>,
    metadata: Arc<MetadataService<YtdlpAdapter>>,
    streaming: Arc<StreamingService<YtdlpAdapter>>,
    import: Arc<crate::youtube::import::ImportService<YtdlpAdapter>>,
    download: Arc<crate::youtube::download::DownloadService>,
}

impl std::fmt::Debug for YouTubeManager {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YouTubeManager")
            .field("request_tx", &self.request_tx)
            .field("event_tx", &self.event_tx)
            .finish_non_exhaustive()
    }
}

impl YouTubeManager {
    pub fn new(
        db_path: &std::path::Path,
        cache_dir: Option<PathBuf>,
        lyrics_dir: Option<String>,
        lyrics_sub_langs: String,
        address: MpdAddress,
        password: Option<MpdPassword>,
    ) -> Result<Self> {
        let db = Arc::new(Mutex::new(Database::new(db_path)?));
        let (request_tx, request_rx) = bounded(100);
        let (event_tx, event_rx) = bounded(100);
        let metadata_cache = Arc::new(RwLock::new(LruCache::new(
            NonZeroUsize::new(METADATA_CACHE_SIZE).expect("METADATA_CACHE_SIZE is non-zero"),
        )));
        let url_cache = Arc::new(RwLock::new(ExpiringCache::new(
            NonZeroUsize::new(URL_CACHE_SIZE).expect("URL_CACHE_SIZE is non-zero"),
        )));
        let client = Arc::new(YtdlpAdapter);

        // Limit concurrent yt-dlp processes to avoid system strain
        let (permit_tx, permit_rx) = bounded(CONCURRENT_YTDLP_LIMIT);
        for _ in 0..CONCURRENT_YTDLP_LIMIT {
            let _ = permit_tx.send_blocking(());
        }

        let search = Arc::new(SearchService::new(event_tx.clone(), client.clone()));
        let metadata = Arc::new(MetadataService::with_cache(
            event_tx.clone(),
            db.clone(),
            metadata_cache.clone(),
            client.clone(),
            permit_tx.clone(),
            permit_rx.clone(),
        ));
        let streaming = Arc::new(StreamingService::new(
            event_tx.clone(),
            client.clone(),
            url_cache.clone(),
            metadata_cache.clone(),
            permit_tx.clone(),
            permit_rx.clone(),
        ));
        let import = Arc::new(crate::youtube::import::ImportService::new(
            event_tx.clone(),
            db.clone(),
            address.clone(),
            password.clone(),
            permit_tx.clone(),
            permit_rx.clone(),
            client.clone(),
        ));
        let download = Arc::new(crate::youtube::download::DownloadService::new(
            event_tx.clone(),
            cache_dir,
            lyrics_dir,
            lyrics_sub_langs,
            address,
            password,
            permit_tx.clone(),
            permit_rx.clone(),
        ));

        Ok(Self {
            request_tx,
            request_rx,
            event_tx,
            event_rx,
            db,
            metadata_cache,
            tasks: Arc::new(Mutex::new(Vec::new())),
            search,
            metadata,
            streaming,
            import,
            download,
        })
    }

    pub fn start(&self) {
        let request_rx_clone = self.request_rx.clone();
        let response_tx_clone = self.event_tx.clone();

        let search = self.search.clone();
        let metadata = self.metadata.clone();
        let streaming = self.streaming.clone();
        let import = self.import.clone();
        let download = self.download.clone();

        let mut tasks = self.tasks.lock();

        // Main event router - now uses direct addressing
        tasks.push(smol::spawn(async move {
            log::debug!("YouTube event router started (direct addressing mode)");
            while let Ok(event) = request_rx_clone.recv().await {
                log::trace!("YouTube manager routing request: {event:?}");
                match &event {
                    YouTubeEvent::SearchRequest { .. } | YouTubeEvent::CancelSearch => {
                        search.handle_event(event).await;
                    }
                    YouTubeEvent::MetadataRequest(_)
                    | YouTubeEvent::AddToLibrary(_)
                    | YouTubeEvent::RemoveFromLibrary(_)
                    | YouTubeEvent::RemoveArtistFromLibrary(_)
                    | YouTubeEvent::GetLibraryArtists
                    | YouTubeEvent::GetLibraryTracks(_)
                    | YouTubeEvent::GetLibraryTracksAll
                    | YouTubeEvent::MetadataRefresh => {
                        metadata.handle_event(event).await;
                    }
                    YouTubeEvent::AddToQueue(_)
                    | YouTubeEvent::AddManyToQueue(_)
                    | YouTubeEvent::ReplaceQueue(_)
                    | YouTubeEvent::AddToPlaylist { .. }
                    | YouTubeEvent::CreatePlaylist { .. }
                    | YouTubeEvent::PlaybackError { .. }
                    | YouTubeEvent::RefreshRequest { .. }
                    | YouTubeEvent::ClearQueue => {
                        streaming.handle_event(event).await;
                    }
                    YouTubeEvent::ImportLibrary(_) | YouTubeEvent::ImportPlaylists(_) => {
                        import.handle_event(event).await;
                    }
                    YouTubeEvent::Download(_)
                    | YouTubeEvent::DownloadMany(_)
                    | YouTubeEvent::DownloadThumbnail(_)
                    | YouTubeEvent::GetLyrics(_) => {
                        download.handle_event(event).await;
                    }
                    _ => {
                        let _ = response_tx_clone.send(event).await;
                    }
                }
            }
            log::debug!("YouTube event router stopped");
        }));

        // Periodic metadata refresh trigger
        let sender = self.request_tx.clone();
        tasks.push(smol::spawn(async move {
            // Wait after startup before first refresh to avoid initial spike
            smol::Timer::after(STARTUP_DELAY).await;
            loop {
                let _ = sender.send(YouTubeEvent::MetadataRefresh).await;
                smol::Timer::after(METADATA_REFRESH_INTERVAL).await;
            }
        }));
    }

    pub fn sender(&self) -> Sender<YouTubeEvent> {
        self.request_tx.clone()
    }

    pub fn receiver(&self) -> Receiver<YouTubeEvent> {
        self.event_rx.clone()
    }

    pub fn get_cached_metadata(&self, youtube_id: &YouTubeId) -> Option<YouTubeTrack> {
        let track = {
            let mut cache = self.metadata_cache.write();
            if let Some(track) = cache.get(youtube_id) {
                Some(track.clone())
            } else if let Ok(Some(track)) = self.db.lock().get_track(youtube_id.as_str()) {
                cache.put(youtube_id.clone(), track.clone());
                Some(track)
            } else {
                None
            }
        };

        if let Some(ref t) = track {
            if !t.is_fresh() {
                let _ =
                    self.request_tx.try_send(YouTubeEvent::MetadataRequest(youtube_id.to_string()));
            }
        } else {
            let _ = self.request_tx.try_send(YouTubeEvent::MetadataRequest(youtube_id.to_string()));
        }
        track
    }

    pub fn enrich_songs(&self, songs: &mut [Song]) {
        for song in songs {
            self.enrich_song(song);
        }
    }

    pub fn enrich_song(&self, song: &mut Song) {
        if let Some(yt_id) = song.youtube_id() {
            if let Some(metadata) = self.get_cached_metadata(&yt_id) {
                song.enrich_from_youtube(&metadata);
            }
        }
    }

    pub async fn resolve_stream_url(&self, youtube_id: &str) -> Result<String> {
        self.streaming.get_streaming_url(&YouTubeId::new(youtube_id)).await
    }

    pub fn list_library_tracks(&self) -> Result<Vec<YouTubeTrack>> {
        self.db.lock().list_tracks()
    }
}
