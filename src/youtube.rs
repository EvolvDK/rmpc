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
    core::data_store::models::YouTubeVideo,
    shared::events::{AppEvent, WorkDone},
};

/// Représente les métadonnées brutes d'une vidéo YouTube, désérialisées depuis yt-dlp.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct YtDlpVideoInfo {
    // Champs pour l'affichage brut
    pub id: String,
    pub title: String,
    pub channel: String,
    pub duration: f64,
    pub thumbnail: Option<String>,

    // Champs pour le mappage prioritaire
    track: Option<String>,
    artist: Option<String>,
    artists: Option<Vec<String>>,
    album: Option<String>,
    album_artist: Option<String>,
    album_artists: Option<Vec<String>>,
    creator: Option<String>,
    creators: Option<Vec<String>>,
    uploader: Option<String>,
    uploader_id: Option<String>,
    fulltitle: Option<String>,
    alt_title: Option<String>,
    display_id: Option<String>,
}

impl From<YtDlpVideoInfo> for YouTubeVideo {
    fn from(info: YtDlpVideoInfo) -> Self {
        let title = info
            .track
            .or(info.title.clone().into())
            .or(info.fulltitle)
            .or(info.alt_title)
            .or(info.display_id)
            .unwrap_or_default();

        let artist = info
            .artist
            .or(info.album_artist)
            .or(info.creator)
            .or(info.uploader)
            .or(info.channel.clone().into())
            .or(info.uploader_id)
            .unwrap_or_default();

        Self {
            youtube_id: info.id,
            title,
            channel: artist,
            album: info.album,
            duration_secs: info.duration as u32,
            thumbnail_url: info.thumbnail,
        }
    }
}

impl YouTubeVideo {
    /// Converts a YouTubeVideo into a generic Song structure for previewing.
    pub(crate) fn to_song_for_preview(&self) -> crate::mpd::commands::Song {
        use crate::mpd::commands::metadata_tag::MetadataTag;
        use std::collections::HashMap;

        let mut metadata = HashMap::new();
        metadata.insert("title".to_string(), MetadataTag::Single(self.title.clone()));
        metadata.insert("artist".to_string(), MetadataTag::Single(self.channel.clone()));
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
    searches: Mutex<HashMap<String, (Instant, Vec<YtDlpVideoInfo>)>>,
    stream_urls: Mutex<HashMap<String, (Instant, String)>>,
    video_info: Mutex<HashMap<String, (Instant, YtDlpVideoInfo)>>,
}

static CACHE: Lazy<Cache> = Lazy::new(|| Cache {
    searches: Default::default(),
    stream_urls: Default::default(),
    video_info: Default::default(),
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
        if let Ok(video_info) = serde_json::from_str::<YtDlpVideoInfo>(&line) {
            if ttl > Duration::ZERO {
                results_for_cache.push(video_info.clone());
            }
            event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSearchResult {
                video_info,
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
/// * `video_id` - L'ID de la vidéo YouTube.
/// * `ttl` - La durée de vie du cache.
pub async fn get_stream_url(video_id: &str, ttl: Duration) -> Result<String> {
    if ttl > Duration::ZERO {
        if let Some((created, url)) = CACHE.stream_urls.lock().unwrap().get(video_id) {
            if created.elapsed() < ttl {
                return Ok(url.clone());
            }
        }
    }

    let output = Command::new("yt-dlp")
        .arg("-g") // Get URL
        .arg("-f") // Format
        .arg("bestaudio")
        .arg(format!("https://www.youtube.com/watch?v={}", video_id))
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
            .insert(video_id.to_string(), (Instant::now(), url.clone()));
    }

    Ok(url)
}

pub async fn get_stream_url_and_metadata(video_id: &str) -> Result<(String, YouTubeVideo)> {
    let output = Command::new("yt-dlp")
        .arg("-g")
        .arg("--dump-json")
        .arg("-f")
        .arg("bestaudio")
        .arg(format!("https://www.youtube.com/watch?v={}", video_id))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("yt-dlp get_stream_url_and_metadata failed: {}", stderr));
    }

    let output_str = String::from_utf8(output.stdout)?;
    let mut lines = output_str.lines();
    let url = lines.next().context("yt-dlp did not return a URL")?.to_string();
    let metadata_json = lines.next().context("yt-dlp did not return JSON metadata")?;

    let video_info: YtDlpVideoInfo = serde_json::from_str(metadata_json)?;

    Ok((url, video_info.into()))
}

pub async fn get_video_info(video_id: &str, ttl: Duration) -> Result<Option<YouTubeVideo>> {
    if ttl > Duration::ZERO {
        if let Some((created, video_info)) = CACHE.video_info.lock().unwrap().get(video_id) {
            if created.elapsed() < ttl {
                return Ok(Some(video_info.clone().into()));
            }
        }
    }

    let output = Command::new("yt-dlp")
        .arg("--dump-json")
        .arg(format!("https://www.youtube.com/watch?v={}", video_id))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("yt-dlp get_video_info for {} failed: {}", video_id, stderr);
        return Ok(None);
    }

    let video_info: YtDlpVideoInfo = serde_json::from_slice(&output.stdout)?;

    if ttl > Duration::ZERO {
        CACHE
            .video_info
            .lock()
            .unwrap()
            .insert(video_id.to_string(), (Instant::now(), video_info.clone()));
    }

    Ok(Some(video_info.into()))
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
