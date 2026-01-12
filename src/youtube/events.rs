use crate::youtube::models::{FilterMode, SearchResult, YouTubeTrack};
use std::sync::Arc;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum YouTubeEvent {
    // Search
    SearchRequest {
        query: String,
        filter_mode: FilterMode,
        limit: usize,
    },
    SearchStarted,
    SearchResult(SearchResult),
    SearchFinished,
    SearchError(String),
    CancelSearch,
    YouTubeError(String),

    // Library
    AddToLibrary(YouTubeTrack),
    RemoveFromLibrary(String),       // youtube_id
    RemoveArtistFromLibrary(String), // artist name
    LibraryUpdated,
    GetLibraryArtists,
    GetLibraryTracks(String), // artist name
    GetLibraryTracksAll,
    LibraryArtistsAvailable(Arc<[String]>),
    LibraryTracksAvailable {
        artist: String,
        tracks: Arc<[YouTubeTrack]>,
    },
    AllLibraryTracksAvailable(Arc<[YouTubeTrack]>),

    // Playback/Queue
    AddToQueue(YouTubeTrack),
    AddManyToQueue(Arc<[YouTubeTrack]>),
    AddManyToQueueResult {
        added: usize,
        skipped: usize,
    },
    ReplaceQueue(Arc<[YouTubeTrack]>),
    ClearQueue,
    QueueUrl(String),
    QueueUrlReplace {
        url: String,
        youtube_id: String,
        pos: Option<u32>,
        play_after_replace: bool,
    },
    PlaybackError {
        youtube_id: String,
        pos: Option<u32>,
        _message: String,
        play_after_refresh: bool,
    },
    RefreshRequest {
        youtube_id: String,
        pos: Option<u32>,
        play_after_refresh: bool,
    },

    // Metadata
    MetadataRequest(String), // youtube_id
    MetadataAvailable(Arc<YouTubeTrack>),
    MetadataRefresh,

    // Lyrics
    GetLyrics(crate::mpd::commands::Song),

    // Playlists
    AddToPlaylist {
        playlist: String,
        tracks: Arc<[YouTubeTrack]>,
    },
    AddToPlaylistResult {
        playlist: String,
        urls: Arc<[String]>,
    },
    CreatePlaylist {
        name: String,
        tracks: Arc<[YouTubeTrack]>,
    },
    CreatePlaylistResult {
        name: String,
        urls: Arc<[String]>,
    },

    // Download
    Download(YouTubeTrack),
    DownloadMany(Arc<[YouTubeTrack]>),
    DownloadThumbnail(crate::youtube::models::YouTubeId),
    ThumbnailDownloaded {
        youtube_id: crate::youtube::models::YouTubeId,
        path: std::path::PathBuf,
    },

    // Import
    ImportLibrary(String),   // path to CSV
    ImportPlaylists(String), // path to folder
    ImportProgress {
        current: usize,
        total: usize,
        message: String,
    },
    ImportFinished {
        success: usize,
        failed: usize,
    },
}
