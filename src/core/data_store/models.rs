#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YouTubeVideo {
    pub youtube_id: String,
    pub title: String,
    pub channel: String,
    pub album: Option<String>,
    pub duration_secs: u32,
    pub thumbnail_url: Option<String>,
}
