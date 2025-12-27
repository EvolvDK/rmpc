// src/youtube/error.rs
use thiserror::Error;
use crate::youtube::security::validation::ValidationError;
use std::time::Duration;

/// A specific error enum for all YouTube service and client operations.
#[derive(Debug, Error)]
pub enum YouTubeError {
	#[error("Validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("Command failed: {0}")]
    CommandFailed(String),
    #[error("Video is unavailable (private, deleted, or region-locked)")]
    VideoUnavailable,
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Failed to parse response from yt-dlp: {0}")]
    ResponseParseError(String),
    #[error("yt-dlp executable not found")]
    YtDlpNotFound,
    #[error("Operation timed out after {0:?}")]
    Timeout(Duration),
    #[error("Rate limit exceeded")]
    RateLimitExceeded,
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Internal error: {0}")]
    Internal(String),
    #[error("Cache error: {0}")]
    Cache(String),
    #[error("I/O error during command execution: {0}")]
    Io(#[from] std::io::Error),
}

impl Clone for YouTubeError {
	fn clone(&self) -> Self {
		match self {
			Self::Validation(v) => Self::Validation(v.clone()),
			Self::CommandFailed(s) => Self::CommandFailed(s.clone()),
			Self::VideoUnavailable => Self::VideoUnavailable,
			Self::NetworkError(s) => Self::NetworkError(s.clone()),
			Self::ResponseParseError(s) => Self::ResponseParseError(s.clone()),
			Self::YtDlpNotFound => Self::YtDlpNotFound,
			Self::Timeout(d) => Self::Timeout(*d),
			Self::RateLimitExceeded => Self::RateLimitExceeded,
			Self::InvalidRequest(s) => Self::InvalidRequest(s.clone()),
			Self::Internal(s) => Self::Internal(s.clone()),
			Self::Cache(s) => Self::Cache(s.clone()),
			Self::Io(e) => Self::Io(std::io::Error::new(e.kind(), e.to_string())),
        }
    }
}

impl From<anyhow::Error> for YouTubeError {
    fn from(err: anyhow::Error) -> Self {
        YouTubeError::Internal(err.to_string())
    }
}

// Helper to create a CommandFailed error from a std::io::Error
impl YouTubeError {
    pub fn from_io_spawn(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::NotFound {
            Self::YtDlpNotFound
        } else {
            Self::Io(e)
        }
    }
}
