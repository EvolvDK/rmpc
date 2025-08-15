use std::{collections::HashSet, fs, path::Path};

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
}

pub struct DataStore {
    conn: Connection,
}

impl DataStore {
    const DB_VERSION: u32 = 1;

    /// Creates a new `DataStore` instance, opening or creating the database file
    /// at the default application config location.
    pub fn new() -> Result<Self, DataStoreError> {
        let config_dir = dirs::config_dir().ok_or(DataStoreError::ConfigDirNotFound)?;
        let db_dir = config_dir.join("rmpc");
        fs::create_dir_all(&db_dir)?;
        let db_path = db_dir.join("rmpc.db");

        let mut conn = Self::open_database(&db_path)?;

        Self::apply_migrations(&mut conn)?;

        Ok(Self { conn })
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

        if user_version < Self::DB_VERSION {
            if user_version == 0 {
                let tx = conn.transaction()?;

                tx.execute_batch(
                    "
                    -- Pour les métadonnées des pistes dans la file d'attente MPD
                    CREATE TABLE IF NOT EXISTS queue_youtube_metadata (
                        song_id     INTEGER PRIMARY KEY, -- ID de la chanson dans la file d'attente MPD
                        youtube_id  TEXT NOT NULL
                    );

                    -- Pour la bibliothèque de vidéos YouTube (remplace youtube_library.json)
                    -- Ne stocke que les métadonnées permanentes.
                    CREATE TABLE IF NOT EXISTS videos (
                        youtube_id      TEXT PRIMARY KEY NOT NULL,
                        title           TEXT NOT NULL,
                        channel         TEXT NOT NULL,
                        album           TEXT, -- L'album peut être optionnel
                        duration_secs   INTEGER NOT NULL,
                        thumbnail_url   TEXT -- L'URL de la miniature peut être stockée si elle est stable
                    );

                    -- Pour définir les playlists (remplace la structure de dossiers yt-playlists/)
                    -- Conçue pour contenir à la fois des pistes locales et YouTube.
                    CREATE TABLE IF NOT EXISTS playlists (
                        id      INTEGER PRIMARY KEY AUTOINCREMENT,
                        name    TEXT NOT NULL UNIQUE
                    );

                    -- Pour lier les pistes (locales ou YouTube) aux playlists
                    CREATE TABLE IF NOT EXISTS playlist_items (
                        playlist_id         INTEGER NOT NULL,
                        position            INTEGER NOT NULL, -- Position dans la playlist
                        video_youtube_id    TEXT, -- Pour les pistes YouTube, NULL pour les locales
                        file_path           TEXT, -- Pour les pistes locales, NULL pour les YouTube
                        PRIMARY KEY (playlist_id, position),
                        FOREIGN KEY (playlist_id) REFERENCES playlists(id) ON DELETE CASCADE,
                        FOREIGN KEY (video_youtube_id) REFERENCES videos(youtube_id) ON DELETE CASCADE,
                        CHECK (video_youtube_id IS NOT NULL OR file_path IS NOT NULL) -- S'assure qu'au moins un type de piste est défini
                    );
                    ",
                )?;

                tx.pragma_update(None, "user_version", &Self::DB_VERSION)?;

                tx.commit()?;
            }
        }

        Ok(())
    }

    // --- Queue Metadata ---

    /// Associates a MPD queue song ID with a YouTube video ID.
    ///
    /// If a mapping for the `song_id` already exists, it will be replaced.
    pub fn add_youtube_song_to_queue(
        &self,
        song_id: u32,
        youtube_id: &str,
    ) -> Result<(), DataStoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO queue_youtube_metadata (song_id, youtube_id) VALUES (?1, ?2)",
            (song_id, youtube_id),
        )?;
        Ok(())
    }

    /// Retrieves the YouTube video ID for a given MPD queue song ID.
    pub fn get_youtube_id_for_song(&self, song_id: u32) -> Result<Option<String>, DataStoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT youtube_id FROM queue_youtube_metadata WHERE song_id = ?1")?;
        let result = stmt.query_row([song_id], |row| row.get(0));
        match result {
            Ok(id) => Ok(Some(id)),
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
        let tx = self.conn.transaction()?;
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
        self.conn.execute("DELETE FROM queue_youtube_metadata", [])?;
        Ok(())
    }

    /// Returns a set of all YouTube video IDs currently in the queue.
    ///
    /// This is useful for checking for duplicates before adding a new video.
    pub fn get_all_queue_youtube_ids(&self) -> Result<HashSet<String>, DataStoreError> {
        let mut stmt = self.conn.prepare("SELECT youtube_id FROM queue_youtube_metadata")?;
        let ids = stmt.query_map([], |row| row.get(0))?;
        let mut result = HashSet::new();
        for id in ids {
            result.insert(id?);
        }
        Ok(result)
    }
}
