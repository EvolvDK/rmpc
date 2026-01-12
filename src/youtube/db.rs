use crate::youtube::error::Result;
use crate::youtube::models::YouTubeTrack;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use std::path::Path;

fn row_to_track(row: &rusqlite::Row) -> rusqlite::Result<YouTubeTrack> {
    let duration_secs: i64 = row.get(4)?;
    Ok(YouTubeTrack {
        youtube_id: row.get(0)?,
        title: row.get(1)?,
        artist: row.get(2)?,
        album: row.get(3)?,
        duration: std::time::Duration::from_secs(duration_secs as u64),
        link: row.get(5)?,
        streaming_url: None,
        thumbnail_url: row.get(6)?,
        added_at: row.get(7)?,
        last_modified_at: row.get(8)?,
    })
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn new(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS youtube_tracks (
                youtube_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                artist TEXT NOT NULL,
                album TEXT,
                duration_secs INTEGER NOT NULL,
                link TEXT NOT NULL,
                thumbnail_url TEXT,
                added_at DATETIME NOT NULL,
                last_modified_at DATETIME NOT NULL
            )",
            [],
        )?;

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_youtube_tracks_artist ON youtube_tracks (artist)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_youtube_tracks_album ON youtube_tracks (album)",
            [],
        )?;

        Ok(())
    }

    pub fn insert_track(&self, track: &YouTubeTrack) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO youtube_tracks (
                youtube_id, title, artist, album, duration_secs, link, thumbnail_url, added_at, last_modified_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                track.youtube_id,
                track.title,
                track.artist,
                track.album,
                i64::try_from(track.duration.as_secs()).unwrap_or(i64::MAX),
                track.link,
                track.thumbnail_url,
                track.added_at,
                track.last_modified_at,
            ],
        )?;
        Ok(())
    }

    pub fn insert_tracks_batch(&mut self, tracks: &[YouTubeTrack]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO youtube_tracks (
                    youtube_id, title, artist, album, duration_secs, link, thumbnail_url, added_at, last_modified_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for track in tracks {
                stmt.execute(params![
                    track.youtube_id,
                    track.title,
                    track.artist,
                    track.album,
                    i64::try_from(track.duration.as_secs()).unwrap_or(i64::MAX),
                    track.link,
                    track.thumbnail_url,
                    track.added_at,
                    track.last_modified_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_track(&self, youtube_id: &str) -> Result<Option<YouTubeTrack>> {
        let mut stmt = self.conn.prepare(
            "SELECT youtube_id, title, artist, album, duration_secs, link, thumbnail_url, added_at, last_modified_at 
             FROM youtube_tracks WHERE youtube_id = ?1",
        )?;
        let mut rows = stmt.query(params![youtube_id])?;

        if let Some(row) = rows.next()? { Ok(Some(row_to_track(row)?)) } else { Ok(None) }
    }

    pub fn delete_track(&self, youtube_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM youtube_tracks WHERE youtube_id = ?1", params![youtube_id])?;
        Ok(())
    }

    pub fn delete_tracks_by_artist(&self, artist: &str) -> Result<()> {
        self.conn.execute("DELETE FROM youtube_tracks WHERE artist = ?1", params![artist])?;
        Ok(())
    }

    pub fn list_tracks(&self) -> Result<Vec<YouTubeTrack>> {
        let mut stmt = self.conn.prepare(
            "SELECT youtube_id, title, artist, album, duration_secs, link, thumbnail_url, added_at, last_modified_at 
             FROM youtube_tracks ORDER BY artist, title",
        )?;
        let rows = stmt.query_map([], row_to_track)?;

        let mut tracks = Vec::new();
        for track in rows {
            tracks.push(track?);
        }
        Ok(tracks)
    }

    pub fn list_stale_tracks(&self, cutoff: DateTime<Utc>) -> Result<Vec<YouTubeTrack>> {
        let mut stmt = self.conn.prepare(
            "SELECT youtube_id, title, artist, album, duration_secs, link, thumbnail_url, added_at, last_modified_at 
             FROM youtube_tracks WHERE last_modified_at < ?1 ORDER BY artist, title",
        )?;
        let rows = stmt.query_map(params![cutoff], row_to_track)?;

        let mut tracks = Vec::new();
        for track in rows {
            tracks.push(track?);
        }
        Ok(tracks)
    }

    pub fn list_artists(&self) -> Result<Vec<String>> {
        let mut stmt =
            self.conn.prepare("SELECT DISTINCT artist FROM youtube_tracks ORDER BY artist")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut artists = Vec::new();
        for artist in rows {
            artists.push(artist?);
        }
        Ok(artists)
    }

    pub fn list_albums(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT album FROM youtube_tracks WHERE album IS NOT NULL ORDER BY album",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut albums = Vec::new();
        for album in rows {
            albums.push(album?);
        }
        Ok(albums)
    }

    pub fn list_tracks_by_artist(&self, artist: &str) -> Result<Vec<YouTubeTrack>> {
        let mut stmt = self.conn.prepare(
            "SELECT youtube_id, title, artist, album, duration_secs, link, thumbnail_url, added_at, last_modified_at 
             FROM youtube_tracks WHERE artist = ?1 ORDER BY title",
        )?;
        let rows = stmt.query_map(params![artist], row_to_track)?;

        let mut tracks = Vec::new();
        for track in rows {
            tracks.push(track?);
        }
        Ok(tracks)
    }

    pub fn list_tracks_by_album(&self, album: &str) -> Result<Vec<YouTubeTrack>> {
        let mut stmt = self.conn.prepare(
            "SELECT youtube_id, title, artist, album, duration_secs, link, thumbnail_url, added_at, last_modified_at 
             FROM youtube_tracks WHERE album = ?1 ORDER BY title",
        )?;
        let rows = stmt.query_map(params![album], row_to_track)?;

        let mut tracks = Vec::new();
        for track in rows {
            tracks.push(track?);
        }
        Ok(tracks)
    }
}
