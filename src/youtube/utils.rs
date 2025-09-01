use crate::youtube::ResolvedYouTubeSong;
use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct YtDlpJsonOutput {
    id: String,
    display_id: Option<String>,

    // Title resolution fields, in order of priority.
    track: Option<String>,
    title: Option<String>,
    fulltitle: Option<String>,
    alt_title: Option<String>,

    // Artist resolution fields, in order of priority.
    artist: Option<String>,
    album_artist: Option<String>,
    creator: Option<String>,
    uploader: Option<String>,
    channel: Option<String>,
    uploader_id: Option<String>,
    
    // Other metadata
    album: Option<String>,
    duration: Option<u64>, // Duration can be null for live streams etc.
    thumbnail: Option<String>,
}

/// Parses a single line of JSON output from yt-dlp into a ResolvedYouTubeSong.
pub fn parse_song_info_json(json_bytes: &[u8]) -> Result<ResolvedYouTubeSong> {
    let parsed: YtDlpJsonOutput = serde_json::from_slice(json_bytes)
        .context("Failed to deserialize JSON from yt-dlp output")?;

    // Apply metadata resolution logic as per regression analysis.
    let title = parsed.track
        .or(parsed.title)
        .or(parsed.fulltitle)
        .or(parsed.alt_title)
        .or(parsed.display_id) // display_id is usually the video ID, a last resort.
        .context("Failed to resolve title from any available field")?;
    
    let artist = parsed.artist
        .or(parsed.album_artist)
        .or(parsed.creator)
        .or(parsed.uploader)
        .or(parsed.channel)
        .or(parsed.uploader_id)
        .context("Failed to resolve artist from any available field")?;

    Ok(ResolvedYouTubeSong {
        youtube_id: parsed.id,
        title,
        artist,
        album: parsed.album,
        duration_secs: parsed.duration.unwrap_or(0) as u32,
        thumbnail_url: parsed.thumbnail,
    })
}
