use crate::core::data_store::models::YouTubeSong;
use serde::{Deserialize, Serialize};

/// Represents a fully resolved YouTube song with all necessary metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedYouTubeSong {
    pub youtube_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: u32,
    pub thumbnail_url: Option<String>,
}

/// Converts the service-layer `ResolvedYouTubeSong` to the data store `YouTubeSong` model.
impl From<ResolvedYouTubeSong> for YouTubeSong {
    fn from(song: ResolvedYouTubeSong) -> Self {
        Self {
            youtube_id: song.youtube_id,
            title: song.title,
            artist: song.artist,
            album: song.album,
            duration_secs: song.duration_secs,
            thumbnail_url: song.thumbnail_url,
        }
    }
}

/// Provides statistics about the cache's state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheStats {
    pub searches_count: usize,
    pub stream_urls_count: usize,
    pub song_info_count: usize,
}
