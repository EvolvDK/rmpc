use crate::youtube::ResolvedYouTubeSong;
use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct YtDlpJsonOutput {
    id: String,
    title: String,
    // Use `serde(alias = ...)` to handle fallback fields declaratively.
    #[serde(alias = "uploader")]
    artist: Option<String>,
    album: Option<String>,
    duration: u64,
    thumbnail: Option<String>,
}

/// Parses a single line of JSON output from yt-dlp into a ResolvedYouTubeSong.
pub fn parse_song_info_json(json_bytes: &[u8]) -> Result<ResolvedYouTubeSong> {
    let parsed: YtDlpJsonOutput = serde_json::from_slice(json_bytes)
        .context("Failed to deserialize JSON from yt-dlp output")?;
    Ok(ResolvedYouTubeSong {
        youtube_id: parsed.id,
        title: parsed.title,
        // Provide a clear error if neither artist nor uploader is present.
        artist: parsed.artist.context("Missing 'artist' or 'uploader' field")?,
        album: parsed.album,
        duration_secs: parsed.duration as u32,
        thumbnail_url: parsed.thumbnail,
    })
}
