pub mod models;

use crate::mpd::commands::Song;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DataStoreError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Could not determine config directory")]
    ConfigDirNotFound,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Playlist name '{0}' is already taken")]
    PlaylistNameTaken(String),
}

pub struct DataStore {
    conn: RefCell<Connection>,
}

impl DataStore {
    const DB_VERSION: u32 = 2;

    /// Creates a new `DataStore` instance, opening or creating the database file
    /// at the default application config location.
    pub fn new() -> Result<Self, DataStoreError> {
        let config_dir = dirs::config_dir().ok_or(DataStoreError::ConfigDirNotFound)?;
        let db_dir = config_dir.join("rmpc");
        fs::create_dir_all(&db_dir)?;
        let db_path = db_dir.join("rmpc.db");

        let mut conn = Self::open_database(&db_path)?;

        Self::apply_migrations(&mut conn)?;

        Ok(Self {
            conn: RefCell::new(conn),
        })
    }

    /// Opens the database and enables foreign key support.
    fn open_database(path: &Path) -> Result<Connection, DataStoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", &true)?;
        Ok(conn)
    }

    /// Applies database migrations to bring the schema to the current version.
    fn apply_migrations(conn: &mut Connection) -> Result<(), DataStoreError> {
        let user_version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

        if user_version < 1 {
            let tx = conn.transaction()?;
            tx.execute_batch(
                "
                    CREATE TABLE IF NOT EXISTS queue_youtube_metadata (
                        song_id     INTEGER PRIMARY KEY,
                        youtube_id  TEXT NOT NULL
                    );
                    CREATE TABLE IF NOT EXISTS songs (
                        youtube_id      TEXT PRIMARY KEY NOT NULL,
                        title           TEXT NOT NULL,
                        artist          TEXT NOT NULL,
                        album           TEXT,
                        duration_secs   INTEGER NOT NULL,
                        thumbnail_url   TEXT
                    );
                    CREATE TABLE IF NOT EXISTS playlists (
                        id      INTEGER PRIMARY KEY AUTOINCREMENT,
                        name    TEXT NOT NULL UNIQUE
                    );
                    CREATE TABLE IF NOT EXISTS playlist_items (
                        playlist_id         INTEGER NOT NULL,
                        position            INTEGER NOT NULL,
                        song_youtube_id    TEXT,
                        file_path           TEXT,
                        PRIMARY KEY (playlist_id, position),
                        FOREIGN KEY (playlist_id) REFERENCES playlists(id) ON DELETE CASCADE,
                        FOREIGN KEY (song_youtube_id) REFERENCES songs(youtube_id) ON DELETE CASCADE,
                        CHECK (song_youtube_id IS NOT NULL OR file_path IS NOT NULL)
                    );
                    ",
            )?;
            tx.pragma_update(None, "user_version", &1)?;
            tx.commit()?;
        }

        if user_version < 2 {
            let tx = conn.transaction()?;
            tx.execute_batch(
                "
                ALTER TABLE queue_youtube_metadata RENAME TO _queue_youtube_metadata_old;

                CREATE TABLE queue_youtube_metadata (
                    song_id     INTEGER PRIMARY KEY,
                    youtube_id  TEXT NOT NULL,
                    updated_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
                );

                INSERT INTO queue_youtube_metadata (song_id, youtube_id)
                SELECT song_id, youtube_id FROM _queue_youtube_metadata_old;

                DROP TABLE _queue_youtube_metadata_old;
                ",
            )?;
            tx.pragma_update(None, "user_version", &Self::DB_VERSION)?;
            tx.commit()?;
        }

        Ok(())
    }

    // --- Queue Metadata ---

    /// Associates a MPD queue song ID with a YouTube song ID.
    ///
    /// If a mapping for the `song_id` already exists, it will be replaced.
    pub fn add_youtube_song_to_queue(
        &self,
        song_id: u32,
        youtube_id: &str,
    ) -> Result<(), DataStoreError> {
        self.conn.borrow().execute(
            "INSERT OR REPLACE INTO queue_youtube_metadata (song_id, youtube_id) VALUES (?1, ?2)",
            (song_id, youtube_id),
        )?;
        Ok(())
    }

    /// Retrieves the YouTube song ID and its update timestamp for a given MPD queue song ID.
    pub fn get_youtube_id_for_song(
        &self,
        song_id: u32,
    ) -> Result<Option<(String, DateTime<Utc>)>, DataStoreError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare("SELECT youtube_id, updated_at FROM queue_youtube_metadata WHERE song_id = ?1")?;
        let result = stmt.query_row([song_id], |row| Ok((row.get(0)?, row.get(1)?)));
        match result {
            Ok((id, updated_at)) => Ok(Some((id, updated_at))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }


    /// Removes metadata for a list of MPD queue song IDs.
    ///
    /// The operation is performed within a single transaction.
    pub fn remove_songs_from_queue(&self, song_ids: &[u32]) -> Result<(), DataStoreError> {
        if song_ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        {
            let mut stmt =
                tx.prepare("DELETE FROM queue_youtube_metadata WHERE song_id = ?1")?;
            for song_id in song_ids {
                stmt.execute([song_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Clears all YouTube metadata from the queue.
    pub fn clear_queue(&self) -> Result<(), DataStoreError> {
        self.conn
            .borrow()
            .execute("DELETE FROM queue_youtube_metadata", [])?;
        Ok(())
    }

    /// Returns a set of all YouTube song IDs currently in the queue.
    ///
    /// This is useful for checking for duplicates before adding a new song.
    pub fn get_all_queue_youtube_ids(&self) -> Result<HashSet<String>, DataStoreError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare("SELECT youtube_id FROM queue_youtube_metadata")?;
        let ids = stmt.query_map([], |row| row.get(0))?;
        let mut result = HashSet::new();
        for id in ids {
            result.insert(id?);
        }
        Ok(result)
    }

    /// Returns a set of all MPD song IDs for which metadata is stored.
    pub fn get_all_queue_song_ids(&self) -> Result<HashSet<u32>, DataStoreError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare("SELECT song_id FROM queue_youtube_metadata")?;
        let ids = stmt.query_map([], |row| row.get(0))?;
        let mut result = HashSet::new();
        for id in ids {
            result.insert(id?);
        }
        Ok(result)
    }

    /// Returns a map of all MPD song IDs to YouTube song IDs.
    pub fn get_all_queue_youtube_mappings(
        &self,
    ) -> Result<HashMap<u32, String>, DataStoreError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare("SELECT song_id, youtube_id FROM queue_youtube_metadata")?;
        let mappings_iter = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;

        let mut result = HashMap::new();
        for mapping in mappings_iter {
            let (song_id, youtube_id) = mapping?;
            result.insert(song_id, youtube_id);
        }
        Ok(result)
    }

    /// Synchronizes the `queue_youtube_metadata` table with the current state of the MPD queue.
    ///
    /// This method performs an "INSERT OR IGNORE" to avoid overwriting existing entries,
    /// preserving their `updated_at` timestamps.
    pub fn sync_queue_from_mpd(&self, songs: &[Song]) -> Result<(), DataStoreError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;

        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO queue_youtube_metadata (song_id, youtube_id) VALUES (?1, ?2)",
            )?;
            for song in songs {
                if let Some(param_start) = song.file.find("&rmpc_yt_id=") {
                    let value_start = param_start + "&rmpc_yt_id=".len();
                    let remainder = &song.file[value_start..];
                    let youtube_id = remainder.split('&').next().unwrap();
                    if !youtube_id.is_empty() {
                        stmt.execute((song.id, youtube_id))?;
                    }
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    // --- Library Management ---

    /// Adds a YouTube song to the library.
    ///
    /// If a song with the same `youtube_id` already exists, it will be replaced.
    pub fn add_song_to_library(&self, song: &models::YouTubeSong) -> Result<(), DataStoreError> {
        self.conn.borrow().execute(
            "
            INSERT OR REPLACE INTO songs (youtube_id, title, artist, album, duration_secs, thumbnail_url)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            (
                &song.youtube_id,
                &song.title,
                &song.artist,
                &song.album,
                &song.duration_secs,
                &song.thumbnail_url,
            ),
        )?;
        Ok(())
    }

    /// Removes a YouTube song from the library.
    pub fn remove_song_from_library(&self, youtube_id: &str) -> Result<(), DataStoreError> {
        self.conn
            .borrow()
            .execute("DELETE FROM songs WHERE youtube_id = ?1", [youtube_id])?;
        Ok(())
    }

    /// Retrieves all YouTube songs from the library.
    pub fn get_all_library_songs(&self) -> Result<Vec<models::YouTubeSong>, DataStoreError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT youtube_id, title, artist, album, duration_secs, thumbnail_url
            FROM songs
            ORDER BY title
        ",
        )?;

        let songs_iter = stmt.query_map([], |row| {
            Ok(models::YouTubeSong {
                youtube_id: row.get(0)?,
                title: row.get(1)?,
                artist: row.get(2)?,
                album: row.get(3)?,
                duration_secs: row.get(4)?,
                thumbnail_url: row.get(5)?,
            })
        })?;

        let mut songs = Vec::new();
        for song in songs_iter {
            songs.push(song?);
        }

        Ok(songs)
    }

    // --- Playlist Management ---

    /// Creates a new, empty playlist.
    ///
    /// Returns the ID of the newly created playlist.
    /// Fails if the playlist name is already taken.
    pub fn create_playlist(&self, name: &str) -> Result<i64, DataStoreError> {
        let conn = self.conn.borrow();
        match conn.execute("INSERT INTO playlists (name) VALUES (?1)", [name]) {
            Ok(_) => Ok(conn.last_insert_rowid()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
            {
                Err(DataStoreError::PlaylistNameTaken(name.to_string()))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Deletes a playlist and all its items.
    pub fn delete_playlist(&self, playlist_id: i64) -> Result<(), DataStoreError> {
        self.conn
            .borrow()
            .execute("DELETE FROM playlists WHERE id = ?1", [playlist_id])?;
        Ok(())
    }

    /// Renames a playlist.
    ///
    /// Fails if the new playlist name is already taken.
    pub fn rename_playlist(&self, playlist_id: i64, new_name: &str) -> Result<(), DataStoreError> {
        match self.conn.borrow().execute(
            "UPDATE playlists SET name = ?1 WHERE id = ?2",
            (new_name, playlist_id),
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
            {
                Err(DataStoreError::PlaylistNameTaken(new_name.to_string()))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Retrieves all playlists with their items.
    pub fn get_all_playlists(&self) -> Result<Vec<models::Playlist>, DataStoreError> {
        let conn = self.conn.borrow();
        let mut stmt =
            conn.prepare("SELECT id, name FROM playlists ORDER BY name COLLATE NOCASE")?;
        let playlists_iter =
            stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;

        let mut playlists = Vec::new();

        for playlist_result in playlists_iter {
            let (id, name) = playlist_result?;
            let items = self.get_playlist_items(id)?;
            playlists.push(models::Playlist { id, name, items });
        }

        Ok(playlists)
    }

    /// Retrieves all items for a given playlist.
    fn get_playlist_items(
        &self,
        playlist_id: i64,
    ) -> Result<Vec<models::PlaylistItem>, DataStoreError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT
                pi.file_path,
                v.youtube_id,
                v.title,
                v.artist,
                v.album,
                v.duration_secs,
                v.thumbnail_url
            FROM playlist_items pi
            LEFT JOIN songs v ON pi.song_youtube_id = v.youtube_id
            WHERE pi.playlist_id = ?1
            ORDER BY pi.position
            ",
        )?;

        let items_iter = stmt.query_map([playlist_id], |row| {
            let file_path: Option<String> = row.get(0)?;
            if let Some(path) = file_path {
                return Ok(models::PlaylistItem::Local(path));
            }

            Ok(models::PlaylistItem::YouTube(models::YouTubeSong {
                youtube_id: row.get(1)?,
                title: row.get(2)?,
                artist: row.get(3)?,
                album: row.get(4)?,
                duration_secs: row.get(5)?,
                thumbnail_url: row.get(6)?,
            }))
        })?;

        let mut items = Vec::new();
        for item in items_iter {
            items.push(item?);
        }
        Ok(items)
    }

    /// Adds a YouTube song to the end of a playlist.
    pub fn add_youtube_song_to_playlist(
        &self,
        playlist_id: i64,
        youtube_id: &str,
    ) -> Result<(), DataStoreError> {
        self.add_playlist_item(playlist_id, Some(youtube_id), None)
    }

    /// Adds a local file to the end of a playlist.
    pub fn add_local_file_to_playlist(
        &self,
        playlist_id: i64,
        file_path: &str,
    ) -> Result<(), DataStoreError> {
        self.add_playlist_item(playlist_id, None, Some(file_path))
    }

    /// Adds an item to the end of a playlist.
    fn add_playlist_item(
        &self,
        playlist_id: i64,
        youtube_id: Option<&str>,
        file_path: Option<&str>,
    ) -> Result<(), DataStoreError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;

        let position: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_items WHERE playlist_id = ?1",
            [playlist_id],
            |row| row.get(0),
        )?;

        tx.execute(
            "INSERT INTO playlist_items (playlist_id, position, song_youtube_id, file_path) VALUES (?1, ?2, ?3, ?4)",
            (playlist_id, position, youtube_id, file_path),
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Removes an item from a specific position in a playlist.
    ///
    /// This will shift all subsequent items up.
    pub fn remove_item_from_playlist(
        &self,
        playlist_id: i64,
        position: usize,
    ) -> Result<(), DataStoreError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        let position = position as i64;

        let changed = tx.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1 AND position = ?2",
            (playlist_id, position),
        )?;

        if changed > 0 {
            tx.execute(
                "UPDATE playlist_items SET position = position - 1 WHERE playlist_id = ?1 AND position > ?2",
                (playlist_id, position),
            )?;
        }

        tx.commit()?;
        Ok(())
    }
}
