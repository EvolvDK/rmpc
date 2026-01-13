use std::{collections::HashMap, time::Duration};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::metadata_tag::MetadataTag;
use crate::mpd::{FromMpd, LineHandled, ParseErrorExt, errors::MpdError};

use crate::youtube::models::{YouTubeId, YouTubeTrack};

#[derive(Default, Serialize, PartialEq, Eq, Clone)]
pub struct Song {
    pub id: u32,
    pub file: String,
    pub duration: Option<Duration>,
    pub metadata: HashMap<String, MetadataTag>,
    pub last_modified: DateTime<Utc>,
    // Option because it is present from mpd 0.24 onwards
    pub added: Option<DateTime<Utc>>,
}

impl From<YouTubeTrack> for Song {
    fn from(yt: YouTubeTrack) -> Self {
        let mut metadata = HashMap::new();
        metadata.insert(Song::TITLE.to_string(), MetadataTag::Single(yt.title));
        metadata.insert(Song::ARTIST.to_string(), MetadataTag::Single(yt.artist));
        if let Some(album) = yt.album {
            metadata.insert(Song::ALBUM.to_string(), MetadataTag::Single(album));
        }

        // We don't have the streaming URL here, so we default to an empty string.
        // The StreamingService will resolve the URL when the song is added to the queue
        // by detecting the #id= fragment.
        let url = "";

        Song {
            file: yt.youtube_id.append_to_url(url),
            metadata,
            duration: Some(yt.duration),
            id: 0,
            last_modified: yt.last_modified_at,
            added: Some(yt.added_at),
        }
    }
}

impl Song {
    pub const TITLE: &'static str = "title";
    pub const ARTIST: &'static str = "artist";
    pub const ALBUM: &'static str = "album";

    pub fn samplerate(&self) -> Option<u32> {
        self.metadata.get("format").and_then(|audio| {
            audio.first().split(':').next().and_then(|rate_str| rate_str.parse().ok())
        })
    }

    pub fn bits(&self) -> Option<u32> {
        self.metadata.get("format").and_then(|audio| {
            audio.first().split(':').nth(1).and_then(|bits_str| bits_str.parse().ok())
        })
    }

    pub fn channels(&self) -> Option<u32> {
        self.metadata.get("format").and_then(|audio| {
            audio.first().split(':').nth(2).and_then(|channels_str| channels_str.parse().ok())
        })
    }

    pub fn is_youtube(&self) -> bool {
        self.youtube_id().is_some()
    }

    pub fn youtube_id(&self) -> Option<YouTubeId> {
        YouTubeId::from_url(&self.file)
    }

    pub fn youtube_expiry(&self) -> Option<u64> {
        crate::youtube::cache::get_youtube_expiry(&self.file)
    }

    pub fn is_youtube_expired(&self) -> bool {
        crate::youtube::cache::is_youtube_url_expired(&self.file) && self.is_youtube()
    }

    pub fn enrich_from_youtube(&mut self, yt: &YouTubeTrack) {
        if self.metadata.is_empty()
            || !self.metadata.contains_key(Self::TITLE)
            || !self.metadata.contains_key(Self::ARTIST)
        {
            self.metadata.insert(Self::TITLE.to_string(), MetadataTag::Single(yt.title.clone()));
            self.metadata.insert(Self::ARTIST.to_string(), MetadataTag::Single(yt.artist.clone()));
            if let Some(album) = &yt.album {
                self.metadata.insert(Self::ALBUM.to_string(), MetadataTag::Single(album.clone()));
            }
        }
        if self.duration.is_none() || self.duration == Some(Duration::ZERO) {
            self.duration = Some(yt.duration);
        }
    }
}

impl std::fmt::Debug for Song {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Song {{ file: {}, title: {:?}, artist: {:?}, id: {}, track: {:?} }}",
            self.file,
            self.metadata.get("title"),
            self.metadata.get("artist"),
            self.id,
            self.metadata.get("track")
        )
    }
}

impl FromMpd for Song {
    fn next_internal(&mut self, key: &str, mut value: String) -> Result<LineHandled, MpdError> {
        match key {
            "file" => self.file = value,
            "id" => self.id = value.parse().logerr(key, &value)?,
            "pos" => {} // Ignored
            "duration" => {
                self.duration = Some(Duration::from_secs_f64(value.parse().logerr(key, &value)?));
            }
            "time" => {} // deprecated or ignored
            "last-modified" => {
                self.last_modified =
                    value.parse().context("Failed to parse date").logerr(key, &value)?;
            }
            "added" => {
                self.added =
                    Some(value.parse().context("Failed to parse date").logerr(key, &value)?);
            }
            key => {
                self.metadata
                    .entry(key.to_owned())
                    .and_modify(|present| match present {
                        MetadataTag::Single(current) => {
                            *present = MetadataTag::Multiple(vec![
                                std::mem::take(current),
                                std::mem::take(&mut value),
                            ]);
                        }
                        MetadataTag::Multiple(items) => {
                            items.push(std::mem::take(&mut value));
                        }
                    })
                    .or_insert(MetadataTag::Single(value));
            }
        }
        Ok(LineHandled::Yes)
    }
}
