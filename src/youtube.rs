use anyhow::{Context, Result};
use crossbeam::channel::Sender;
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::{io::{AsyncBufReadExt, BufReader}, process::{Child, Command}};

use crate::{
    core::data_store::models::YouTubeSong,
    shared::events::{AppEvent, WorkDone},
};

/// Raw metadata from yt-dlp deserialization - kept private to the module
#[derive(Debug, Deserialize, Clone)]
struct YtDlpRawInfo {
    // Champs pour l'affichage brut
    pub id: String,
    pub duration: f64,
    pub thumbnail: Option<String>,

    // Champs pour le mappage prioritaire - tous Option<String>
    // Priorité titre: track -> title -> fulltitle -> alt_title -> display_id
    track: Option<String>,
    title: Option<String>,
    fulltitle: Option<String>,
    alt_title: Option<String>,
    display_id: Option<String>,
    
    // Priorité artiste: artist -> album_artist -> creator -> uploader -> channel -> uploader_id
    artist: Option<String>,
    album_artist: Option<String>,
    creator: Option<String>,
    uploader: Option<String>,
    channel: Option<String>,
    uploader_id: Option<String>,
    
    // Album reste optionnel
    album: Option<String>,
}

/// Resolved YouTube song metadata - public interface with all fields resolved once
#[derive(Debug, Clone)]
pub(crate) struct YtDlpSongInfo {
    pub youtube_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: u32,
    pub thumbnail_url: Option<String>,
}

impl YtDlpSongInfo {
    /// Creates resolved song info from raw yt-dlp metadata
    /// This is where all priority resolution happens - exactly once per song
    pub(crate) fn from_raw(raw: YtDlpRawInfo) -> Self {
        Self {
            youtube_id: raw.id,
            title: resolve_title_priority(&raw),
            artist: resolve_artist_priority(&raw),
            album: raw.album,
            duration_secs: raw.duration as u32,
            thumbnail_url: raw.thumbnail,
        }
    }
}

/// Resolves title according to priority: track -> title -> fulltitle -> alt_title -> display_id
fn resolve_title_priority(raw: &YtDlpRawInfo) -> String {
    raw.track
        .as_ref()
        .or(raw.title.as_ref())
        .or(raw.fulltitle.as_ref())
        .or(raw.alt_title.as_ref())
        .or(raw.display_id.as_ref())
        .cloned()
        .unwrap_or_default()
}

/// Resolves artist according to priority: artist -> album_artist -> creator -> uploader -> channel -> uploader_id
fn resolve_artist_priority(raw: &YtDlpRawInfo) -> String {
    raw.artist
        .as_ref()
        .or(raw.album_artist.as_ref())
        .or(raw.creator.as_ref())
        .or(raw.uploader.as_ref())
        .or(raw.channel.as_ref())
        .or(raw.uploader_id.as_ref())
        .cloned()
        .unwrap_or_default()
}

impl From<YtDlpSongInfo> for YouTubeSong {
    fn from(info: YtDlpSongInfo) -> Self {
        Self {
            youtube_id: info.youtube_id,    // Direct access - no resolution needed
            title: info.title,              // Already resolved
            artist: info.artist,            // Already resolved  
            album: info.album,              // Direct access
            duration_secs: info.duration_secs, // Already converted
            thumbnail_url: info.thumbnail_url,  // Direct access
        }
    }
}

impl YouTubeSong {
    /// Converts a YouTubeSong into a generic Song structure for previewing.
    pub(crate) fn to_song_for_preview(&self) -> crate::mpd::commands::Song {
        use crate::mpd::commands::metadata_tag::MetadataTag;
        use std::collections::HashMap;

        let mut metadata = HashMap::new();
        metadata.insert("title".to_string(), MetadataTag::Single(self.title.clone()));
        metadata.insert("artist".to_string(), MetadataTag::Single(self.artist.clone()));
        
        if let Some(album) = &self.album {
            metadata.insert("album".to_string(), MetadataTag::Single(album.clone()));
        }
        
        metadata.insert("ID".to_string(), MetadataTag::Single(self.youtube_id.clone()));

        crate::mpd::commands::Song {
            file: format!("https://www.youtube.com/watch?v={}", self.youtube_id),
            duration: Some(std::time::Duration::from_secs(self.duration_secs as u64)),
            metadata,
            ..Default::default()
        }
    }
}

#[derive(Debug)]
struct Cache {
    searches: Mutex<HashMap<String, (Instant, Vec<YtDlpSongInfo>)>>,
    stream_urls: Mutex<HashMap<String, (Instant, String)>>,
    song_info: Mutex<HashMap<String, (Instant, YtDlpSongInfo)>>,
}

static CACHE: Lazy<Cache> = Lazy::new(|| Cache {
    searches: Default::default(),
    stream_urls: Default::default(),
    song_info: Default::default(),
});

struct ChildProcessGuard(Child);

impl Drop for ChildProcessGuard {
    fn drop(&mut self) {
        // Tenter de tuer le processus enfant. C'est la meilleure tentative que nous puissions faire.
        // S'il a déjà terminé, cela échouera silencieusement, ce qui est acceptable.
        let _ = self.0.start_kill();
    }
}

/// Exécute une recherche sur YouTube via yt-dlp et retourne une liste de vidéos.
///
/// # Arguments
/// * `query` - La chaîne de caractères pour la recherche.
/// * `ttl` - La durée de vie du cache.
pub async fn search(
    query: &str,
    generation: u64,
    ttl: Duration,
    event_tx: Sender<AppEvent>,
) -> Result<()> {
    // Note: Le cache n'est pas utilisé ici car la logique de génération le rend complexe
    // à gérer correctement sans introduire de potentiels bugs de cohérence.
    // Une implémentation future pourrait stocker les résultats avec leur génération.

    let child = Command::new("yt-dlp")
        .arg("--dump-json")
        .arg(format!("https://music.youtube.com/search?q={}", query))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut guard = ChildProcessGuard(child);
    let cmd = &mut guard.0;

    let stdout = cmd.stdout.take().context("Failed to capture yt-dlp stdout")?;
    let mut reader = BufReader::new(stdout).lines();
    let mut results_for_cache = Vec::new();

    while let Some(line) = reader.next_line().await? {
        if let Ok(song_info) = serde_json::from_str::<YtDlpSongInfo>(&line) {
            if ttl > Duration::ZERO {
                results_for_cache.push(song_info.clone());
            }
            event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSearchResult {
                song_info,
                generation,
            })))?;
        }
    }

    let status = guard.0.wait().await?;
    if !status.success() {
        return Err(anyhow::anyhow!("yt-dlp process exited with non-zero status"));
    }

    if ttl > Duration::ZERO {
        CACHE
            .searches
            .lock()
            .unwrap()
            .insert(query.to_string(), (Instant::now(), results_for_cache));
    }

    event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSearchFinished { generation })))?;

    Ok(())
}

/// Récupère l'URL de streaming pour le meilleur format audio d'une vidéo YouTube.
///
/// # Arguments
/// * `song_id` - L'ID de la vidéo YouTube.
/// * `ttl` - La durée de vie du cache.
pub async fn get_stream_url(song_id: &str, ttl: Duration) -> Result<String> {
    if ttl > Duration::ZERO {
        if let Some((created, url)) = CACHE.stream_urls.lock().unwrap().get(song_id) {
            if created.elapsed() < ttl {
                return Ok(url.clone());
            }
        }
    }

    let output = Command::new("yt-dlp")
        .arg("-g") // Get URL
        .arg("-f") // Format
        .arg("bestaudio")
        .arg(format!("https://www.youtube.com/watch?v={}", song_id))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("yt-dlp get_stream_url failed: {}", stderr));
    }

    let url = String::from_utf8(output.stdout)?.trim().to_string();
    if url.is_empty() {
        return Err(anyhow::anyhow!("yt-dlp returned an empty URL."));
    }

    if ttl > Duration::ZERO {
        CACHE
            .stream_urls
            .lock()
            .unwrap()
            .insert(song_id.to_string(), (Instant::now(), url.clone()));
    }

    Ok(url)
}

pub async fn get_song_info(song_id: &str, ttl: Duration) -> Result<Option<YouTubeSong>> {
    if ttl > Duration::ZERO {
        if let Some((created, song_info)) = CACHE.song_info.lock().unwrap().get(song_id) {
            if created.elapsed() < ttl {
                return Ok(Some(song_info.clone().into()));
            }
        }
    }

    let output = Command::new("yt-dlp")
        .arg("--dump-json")
        .arg(format!("https://www.youtube.com/watch?v={}", song_id))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("yt-dlp get_song_info for {} failed: {}", song_id, stderr);
        return Ok(None);
    }

    let song_info: YtDlpSongInfo = serde_json::from_slice(&output.stdout)?;

    if ttl > Duration::ZERO {
        CACHE
            .song_info
            .lock()
            .unwrap()
            .insert(song_id.to_string(), (Instant::now(), song_info.clone()));
    }

    Ok(Some(song_info.into()))
}

/// Ajoute l'ID YouTube permanent à une URL de streaming comme paramètre de requête.
///
/// Cela permet la persistance de l'ID à travers les redémarrages de MPD,
/// car le champ `file` est conservé.
pub fn append_youtube_id_to_url(mut url: String, youtube_id: &str) -> String {
    url.push_str("&rmpc_yt_id=");
    url.push_str(youtube_id);
    url
}
