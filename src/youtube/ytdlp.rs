use crate::youtube::error::{Result, YouTubeError};
use crate::youtube::models::{SearchResult, YouTubeId, YouTubeTrack};
use anyhow::Context;
use futures_lite::{AsyncBufReadExt, Stream, StreamExt, io::BufReader};
use serde::Deserialize;
use smol::process::Command;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

#[derive(Deserialize)]
struct YtdlpSearchResult {
    id: Option<String>,
    display_id: Option<String>,
    track: Option<String>,
    title: Option<String>,
    fulltitle: Option<String>,
    alt_title: Option<String>,
    creator: Option<String>,
    album_artist: Option<String>,
    uploader: Option<String>,
    channel: Option<String>,
    uploader_id: Option<String>,
    album: Option<String>,
    artist: Option<String>,
    duration: Option<f64>,
    thumbnail: Option<String>,
}

pub trait YouTubeClient: Send + Sync {
    fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Pin<Box<dyn futures_lite::Stream<Item = Result<SearchResult>> + Send>>>;
    fn get_streaming_url(
        &self,
        youtube_id: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'static>>;
    fn get_metadata(
        &self,
        youtube_id: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<YouTubeTrack>> + Send + 'static>>;
}

pub struct YtdlpAdapter;

impl YouTubeClient for YtdlpAdapter {
    fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Pin<Box<dyn futures_lite::Stream<Item = Result<SearchResult>> + Send>>> {
        Self::search(query, limit)
    }

    fn get_streaming_url(
        &self,
        youtube_id: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'static>> {
        let youtube_id = youtube_id.to_string();
        Box::pin(async move { Self::get_streaming_url(&youtube_id).await })
    }

    fn get_metadata(
        &self,
        youtube_id: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<YouTubeTrack>> + Send + 'static>> {
        let youtube_id = youtube_id.to_string();
        Box::pin(async move { Self::get_metadata(&youtube_id).await })
    }
}

impl YtdlpAdapter {
    pub fn parse_search_result(line: &str) -> Result<SearchResult> {
        log::trace!("yt-dlp search output line: {line}");
        let json: YtdlpSearchResult = serde_json::from_str(line)?;

        let youtube_id = json.id.clone().or(json.display_id.clone()).unwrap_or_default();

        let title = json
            .track
            .clone()
            .or(json.title.clone())
            .or(json.fulltitle.clone())
            .or(json.alt_title.clone())
            .or(json.display_id.clone())
            .unwrap_or_else(|| "Unknown Title".to_string());

        let creator = json
            .creator
            .clone()
            .or(json.artist.clone())
            .or(json.album_artist.clone())
            .or(json.uploader.clone())
            .or(json.channel.clone())
            .or(json.uploader_id.clone())
            .unwrap_or_else(|| "Unknown Creator".to_string());

        Ok(SearchResult {
            youtube_id: YouTubeId::new(youtube_id),
            title,
            creator,
            album: json.album,
            duration: Duration::from_secs_f64(json.duration.unwrap_or(0.0)),
            thumbnail_url: json.thumbnail,
        })
    }

    #[cfg(test)]
    pub fn parse_metadata(youtube_id: &str, json_data: &[u8]) -> Result<YouTubeTrack> {
        let json: YtdlpSearchResult = serde_json::from_slice(json_data)?;

        let title = json
            .track
            .clone()
            .or(json.title.clone())
            .or(json.fulltitle.clone())
            .or(json.alt_title.clone())
            .or(json.display_id.clone())
            .unwrap_or_else(|| "Unknown Title".to_string());

        let artist = json
            .artist
            .clone()
            .or(json.album_artist.clone())
            .or(json.creator.clone())
            .or(json.uploader.clone())
            .or(json.channel.clone())
            .or(json.uploader_id.clone())
            .unwrap_or_else(|| "Unknown Artist".to_string());

        Ok(YouTubeTrack {
            youtube_id: YouTubeId::new(youtube_id),
            title,
            artist,
            album: json.album,
            duration: Duration::from_secs_f64(json.duration.unwrap_or(0.0)),
            link: format!("https://www.youtube.com/watch?v={youtube_id}"),
            thumbnail_url: json.thumbnail,
            added_at: chrono::Utc::now(),
            last_modified_at: chrono::Utc::now(),
        })
    }

    pub fn search(
        query: &str,
        limit: usize,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<SearchResult>> + Send>>> {
        let encoded_query = urlencoding::encode(query);
        let url = format!("https://music.youtube.com/search?q={encoded_query}");
        let limit_arg = format!("1:{limit}");

        log::debug!("Executing yt-dlp search with URL: {url} and limit: {limit}");
        let mut child = Command::new("yt-dlp")
            .kill_on_drop(true)
            .args([
                "--no-warnings",
                "--quiet",
                "--dump-json",
                "--ignore-errors",
                "--no-check-formats",
                "--playlist-items",
                limit_arg.as_str(),
                &url,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| YouTubeError::SpawnFailed(e.to_string()))?;

        let stdout =
            child.stdout.take().ok_or_else(|| YouTubeError::CaptureFailed("stdout".to_string()))?;
        let stderr =
            child.stderr.take().ok_or_else(|| YouTubeError::CaptureFailed("stderr".to_string()))?;

        // Spawn a task to log stderr
        smol::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    log::error!("yt-dlp stderr: {trimmed}");
                }
                line.clear();
            }
        })
        .detach();

        let reader = BufReader::new(stdout).lines();

        let stream =
            futures_lite::stream::unfold((child, reader), |(mut child, mut reader)| async move {
                match reader.next().await {
                    Some(Ok(line)) => {
                        let res = Self::parse_search_result(&line);
                        Some((res, (child, reader)))
                    }
                    Some(Err(e)) => Some((Err(e.into()), (child, reader))),
                    None => {
                        // Reap child status
                        let _ = child.status().await;
                        None
                    }
                }
            });

        Ok(Box::pin(stream))
    }

    pub async fn get_streaming_url(youtube_id: &str) -> Result<String> {
        let output = Command::new("yt-dlp")
            .args([
                "-g",
                "-f",
                "bestaudio[protocol^=https]",
                &format!("https://www.youtube.com/watch?v={youtube_id}"),
            ])
            .output()
            .await
            .context("Failed to spawn yt-dlp for streaming URL")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(YouTubeError::YtdlpFailed(stderr.to_string()));
        }

        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if url.is_empty() {
            return Err(YouTubeError::EmptyOutput);
        }

        Ok(url)
    }

    pub async fn get_metadata(youtube_id: &str) -> Result<YouTubeTrack> {
        let output = Command::new("yt-dlp")
            .args(["--dump-json", &format!("https://www.youtube.com/watch?v={youtube_id}")])
            .output()
            .await
            .context("Failed to spawn yt-dlp for metadata")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(YouTubeError::YtdlpFailed(stderr.to_string()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Self::parse_search_result(&stdout).map(YouTubeTrack::from)
    }
}
