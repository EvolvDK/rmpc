pub mod cache;
pub mod constants;
pub mod db;
pub mod download;
pub mod error;
pub mod events;
pub mod import;
pub mod manager;
pub mod metadata;
pub mod models;
pub mod search;
pub mod streaming;
pub mod utils;
pub mod ytdlp;

#[cfg(test)]
mod tests;

use crate::mpd::commands::Song;
use std::path::Path;

/// Enrich a list of songs with metadata from the local database if available.
/// This is intended for use where the full `YouTubeManager` is not available (e.g. CLI commands).
pub fn enrich_songs_from_db(songs: &mut [Song], db_path: &Path) {
    if !db_path.exists() {
        return;
    }
    if let Ok(db) = crate::youtube::db::Database::new(db_path) {
        for song in songs {
            if let Some(yt_id) = song.youtube_id() {
                if let Ok(Some(yt)) = db.get_track(yt_id.as_str()) {
                    song.enrich_from_youtube(&yt);
                }
            }
        }
    }
}
