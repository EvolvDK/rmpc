use chrono::{DateTime, Utc};
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::youtube::constants::YOUTUBE_ID_FRAGMENT;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YouTubeTrack {
    pub youtube_id: YouTubeId,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration: Duration,
    pub link: String,
    pub streaming_url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub added_at: DateTime<Utc>,
    pub last_modified_at: DateTime<Utc>,
}

impl YouTubeTrack {
    pub fn is_fresh(&self) -> bool {
        Utc::now() - self.last_modified_at
            < chrono::Duration::days(crate::youtube::constants::STALE_METADATA_THRESHOLD_DAYS)
    }
}

impl Default for YouTubeTrack {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            youtube_id: YouTubeId::new(""),
            title: String::new(),
            artist: String::new(),
            album: None,
            duration: Duration::from_secs(0),
            link: String::new(),
            streaming_url: None,
            thumbnail_url: None,
            added_at: now,
            last_modified_at: now,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Hash,
    Default,
    derive_more::Display,
    derive_more::AsRef,
    derive_more::Deref,
)]
#[as_ref(str)]
pub struct YouTubeId(String);

impl YouTubeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn append_to_url(&self, url: &str) -> String {
        format!("{url}{}{}", YOUTUBE_ID_FRAGMENT, self.0)
    }

    pub fn from_url(url: &str) -> Option<Self> {
        extract_youtube_id(url).map(|s| Self(s.to_string()))
    }

    pub fn from_any(s: &str) -> Option<Self> {
        if s.len() == 11 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Some(Self(s.to_string()));
        }

        // Try extracting from #id= fragment first
        if let Some(id) = extract_youtube_id(s) {
            return Some(Self(id.to_string()));
        }

        // Try parsing as standard YouTube URL
        if let Ok(url) = url::Url::parse(s) {
            let host = url.host_str().unwrap_or_default();
            if host.contains("youtube.com") || host.contains("youtu.be") {
                if let Some(v) =
                    url.query_pairs().find(|(k, _)| k == "v").map(|(_, v)| v.to_string())
                {
                    return Some(Self(v));
                }
                if let Some(last) = url.path_segments().and_then(|mut s| s.next_back()) {
                    if last.len() == 11 {
                        return Some(Self(last.to_string()));
                    }
                }
            }
        }
        None
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<YouTubeId> for String {
    fn from(id: YouTubeId) -> Self {
        id.0
    }
}

impl<'a> From<&'a YouTubeId> for std::borrow::Cow<'a, str> {
    fn from(id: &'a YouTubeId) -> Self {
        std::borrow::Cow::Borrowed(&id.0)
    }
}

impl ToSql for YouTubeId {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.as_str()))
    }
}

impl FromSql for YouTubeId {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        Ok(Self(String::column_result(value)?))
    }
}

impl PartialEq<String> for YouTubeId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl PartialEq<&str> for YouTubeId {
    fn eq(&self, other: &&str) -> bool {
        &self.0 == other
    }
}

impl PartialEq<YouTubeId> for String {
    fn eq(&self, other: &YouTubeId) -> bool {
        self == &other.0
    }
}

impl PartialEq<YouTubeId> for &str {
    fn eq(&self, other: &YouTubeId) -> bool {
        *self == other.0
    }
}

pub fn extract_youtube_id(url: &str) -> Option<&str> {
    let id_start = url.find(YOUTUBE_ID_FRAGMENT)? + YOUTUBE_ID_FRAGMENT.len();
    let id_end = url[id_start..].find('&').map_or(url.len(), |pos| id_start + pos);
    Some(&url[id_start..id_end])
}

impl From<SearchResult> for YouTubeTrack {
    fn from(res: SearchResult) -> Self {
        let now = Utc::now();
        Self {
            youtube_id: res.youtube_id.clone(),
            title: res.title,
            artist: res.creator,
            album: res.album,
            duration: res.duration,
            link: format!("https://www.youtube.com/watch?v={}", res.youtube_id),
            streaming_url: None,
            thumbnail_url: res.thumbnail_url,
            added_at: now,
            last_modified_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchResult {
    pub youtube_id: YouTubeId,
    pub title: String,
    pub creator: String,
    pub album: Option<String>,
    pub duration: Duration,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum FilterMode {
    #[default]
    Fuzzy,
    Contains,
    StartsWith,
    Exact,
    Regex,
}