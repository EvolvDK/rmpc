#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YouTubeSong {
    pub youtube_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: u32,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistItem {
    YouTube(YouTubeSong),
    Local(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub items: Vec<PlaylistItem>,
}
