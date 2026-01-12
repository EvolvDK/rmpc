use thiserror::Error;

#[derive(Debug, Error)]
pub enum YouTubeError {
    #[error("Network error: {0}")]
    Network(#[from] std::io::Error),

    #[error("yt-dlp failed: {0}")]
    YtdlpFailed(String),

    #[error("Failed to spawn yt-dlp: {0}")]
    SpawnFailed(String),

    #[error("Failed to capture {0} from yt-dlp")]
    CaptureFailed(String),

    #[error("yt-dlp returned empty output")]
    EmptyOutput,

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    SerdeJson(#[from] serde_json::Error),

    #[error("Anyhow error: {0}")]
    Anyhow(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, YouTubeError>;
