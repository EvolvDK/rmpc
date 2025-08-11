use anyhow::{Context, Result};
use crossbeam::channel::Sender;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::{io::{AsyncBufReadExt, BufReader}, process::Command};

use crate::shared::events::{AppEvent, WorkDone};

pub mod storage;

/// Représente les métadonnées essentielles d'une vidéo YouTube.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct YouTubeVideo {
    pub id: String,
    pub title: String,
    pub duration: f64,
    pub channel: String, // Utilisé comme "artiste"
    pub thumbnail: String,
}

#[derive(Debug)]
struct Cache {
    searches: Mutex<HashMap<String, (Instant, Vec<YouTubeVideo>)>>,
    stream_urls: Mutex<HashMap<String, (Instant, String)>>,
    video_info: Mutex<HashMap<String, (Instant, YouTubeVideo)>>,
}

static CACHE: Lazy<Cache> = Lazy::new(|| Cache {
    searches: Default::default(),
    stream_urls: Default::default(),
    video_info: Default::default(),
});

/// Exécute une recherche sur YouTube via yt-dlp et retourne une liste de vidéos.
///
/// # Arguments
/// * `query` - La chaîne de caractères pour la recherche.
/// * `ttl` - La durée de vie du cache.
pub async fn search(query: &str, ttl: Duration, event_tx: Sender<AppEvent>) -> Result<()> {
    if ttl > Duration::ZERO {
        if let Some((created, videos)) = CACHE.searches.lock().unwrap().get(query) {
            if created.elapsed() < ttl {
                for video in videos {
                    event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSearchResult(
                        video.clone(),
                    ))))?;
                }
                event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSearchFinished)))?;
                return Ok(());
            }
        }
    }

    let mut cmd = Command::new("yt-dlp")
        .arg("--dump-json")
        .arg(format!("ytsearch10:{}", query)) // Recherche les 10 premiers résultats
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let stdout = cmd.stdout.take().context("Failed to capture yt-dlp stdout")?;
    let mut reader = BufReader::new(stdout).lines();
    let mut results_for_cache = Vec::new();

    while let Some(line) = reader.next_line().await? {
        if let Ok(video) = serde_json::from_str::<YouTubeVideo>(&line) {
            if ttl > Duration::ZERO {
                results_for_cache.push(video.clone());
            }
            event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSearchResult(video))))?;
        }
    }

    let status = cmd.wait().await?;
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

    event_tx.send(AppEvent::WorkDone(Ok(WorkDone::YouTubeSearchFinished)))?;

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

pub async fn get_video_info(video_id: &str, ttl: Duration) -> Result<Option<YouTubeVideo>> {
    if ttl > Duration::ZERO {
        if let Some((created, video)) = CACHE.video_info.lock().unwrap().get(video_id) {
            if created.elapsed() < ttl {
                return Ok(Some(video.clone()));
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

    let video: YouTubeVideo = serde_json::from_slice(&output.stdout)?;

    if ttl > Duration::ZERO {
        CACHE
            .video_info
            .lock()
            .unwrap()
            .insert(video_id.to_string(), (Instant::now(), video.clone()));
    }

    Ok(Some(video))
}
