use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use crossterm::event::KeyEvent;
use serde::{Deserialize, Serialize};

use super::{
    ipc::ipc_stream::IpcStream,
    lrc::{LrcIndex, LrcIndexEntry},
    mouse_event::MouseEvent,
    mpd_query::{MpdCommand, MpdQuery, MpdQueryResult, MpdQuerySync},
};
use crate::{
    config::{
        Config,
        Size,
        cli::{Command, RemoteCommandQuery},
        tabs::PaneType,
        theme::UiConfig,
    },
    core::data_store::models::YouTubeSong,
    mpd::commands::IdleEvent,
    ui::UiAppEvent,
    youtube::{
		ResolvedYouTubeSong, error::YouTubeError},
};

/// Context for YouTube stream refresh operations
/// Follows SRP: single responsibility for refresh context data
#[derive(Debug, Clone)]
pub(crate) struct YouTubeStreamContext {
    pub old_song_id: u32,
    pub position: usize,
    pub play_after_refresh: bool,
}

/// Client request types - follows ISP with focused interfaces
#[derive(Debug)]
#[allow(unused)]
pub(crate) enum ClientRequest {
    Query(MpdQuery),
    QuerySync(MpdQuerySync),
    Command(MpdCommand),
}

/// Represent an identified song, which might be missing metadata.
#[derive(Debug, Clone)]
pub enum IdentifiedYouTubeSong {
    Full(YouTubeSong),
    IdOnly(String),
}

/// Work request types - each variant has single responsibility
/// Follows SRP: each request type handles one specific operation
#[derive(Debug)]
#[allow(unused)]
pub(crate) enum WorkRequest {
    IndexLyrics {
        lyrics_dir: String,
    },
    IndexSingleLrc {
        /// Absolute path to the lrc file
        path: PathBuf,
    },
    Command(Command),
    YouTubeSearch {
        query: String,
        generation: u64,
    },
    GetYouTubeStreamUrl {
        song: IdentifiedYouTubeSong,
        context: Option<YouTubeStreamContext>,
    },
    YouTubeGetSongInfo {
        id: String,
        context: Option<YouTubeStreamContext>,
    },
}

/// Work completion results - follows SRP with specific result types
/// Each variant represents completion of a single type of work
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum WorkDone {
    LyricsIndexed { 
        index: LrcIndex 
    },
    SingleLrcIndexed { 
        lrc_entry: Option<LrcIndexEntry> 
    },
    YouTubeSearchResult {
        song_info: ResolvedYouTubeSong,
        generation: u64,
    },
    YouTubeSearchFinished {
        generation: u64,
    },
    YouTubeStreamUrlReady {
        url: String,
        song: YouTubeSong,
        context: Option<YouTubeStreamContext>,
    },
    YouTubeStreamUrlFailed {
        song: IdentifiedYouTubeSong,
        context: Option<YouTubeStreamContext>,
        error: YouTubeError,
    },
    MpdCommandFinished {
        id: &'static str,
        target: Option<PaneType>,
        data: MpdQueryResult,
    },
    YouTubeSongInfoFetched {
        song: YouTubeSong,
        context: Option<YouTubeStreamContext>,
    },
    YouTubeSongInfoFailed {
		id: String,
		error: YouTubeError,
		context: Option<YouTubeStreamContext>,
    },
    None,
}

/// Application event types - central event hub following mediator pattern
/// Follows SRP: each event type represents one specific system event
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum AppEvent {
    // User input events
    UserKeyInput(KeyEvent),
    UserMouseInput(MouseEvent),
    
    // System feedback events
    Status(String, Level, Duration),
    InfoModal {
        message: Vec<String>,
        title: Option<String>,
        size: Option<Size>,
        replacement_id: Option<String>,
    },
    Log(Vec<u8>),
    
    // MPD-related events
    IdleEvent(IdleEvent),
    
    // UI events
    RequestRender,
    Resized {
        columns: u16,
        rows: u16,
    },
    ResizedDebounced {
        columns: u16,
        rows: u16,
    },
    UiEvent(UiAppEvent),
    
    // Work completion events
    WorkDone(Result<WorkDone>),
    
    // Connection events
    Reconnected,
    LostConnection,
    
    // Configuration events
    ConfigChanged {
        config: Box<Config>,
        keep_old_theme: bool,
    },
    ThemeChanged {
        theme: Box<UiConfig>,
    },
    
    // Remote control events
    RemoteSwitchTab {
        tab_name: String,
    },
    TmuxHook {
        hook: String,
    },
    IpcQuery {
        stream: IpcStream,
        targets: Vec<RemoteCommandQuery>,
    },
}

/// Log levels - follows enum best practices with clear hierarchy
#[derive(Debug, Clone, Serialize, Deserialize, Copy, Eq, Hash, PartialEq, Ord, PartialOrd)]
#[allow(dead_code)]
pub enum Level {
    Trace,
    Debug, 
    Info,
    Warn,
    Error,
}

impl Level {
    /// Returns string representation for logging
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG", 
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
    
    /// Checks if this level should be displayed given a minimum level
    pub fn should_display(&self, min_level: Level) -> bool {
        self >= &min_level
    }
}

/// Helper trait for event creation - follows builder pattern principles
pub(crate) trait EventBuilder {
    fn status_info(message: String, duration: Duration) -> AppEvent {
        AppEvent::Status(message, Level::Info, duration)
    }
    
    fn status_warn(message: String, duration: Duration) -> AppEvent {
        AppEvent::Status(message, Level::Warn, duration)
    }
    
    fn status_error(message: String, duration: Duration) -> AppEvent {
        AppEvent::Status(message, Level::Error, duration)
    }
}

// Implement for AppEvent to provide convenient constructors
impl EventBuilder for AppEvent {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_level_ordering() {
        assert!(Level::Error > Level::Warn);
        assert!(Level::Warn > Level::Info);
        assert!(Level::Info > Level::Debug);
        assert!(Level::Debug > Level::Trace);
    }

    #[test]
    fn test_level_display() {
        assert!(Level::Error.should_display(Level::Info));
        assert!(!Level::Debug.should_display(Level::Info));
    }

    #[test]
    fn test_level_as_str() {
        assert_eq!(Level::Error.as_str(), "ERROR");
        assert_eq!(Level::Info.as_str(), "INFO");
    }

    #[test]
    fn test_event_builder() {
        let duration = Duration::from_secs(5);
        let event = AppEvent::status_info("Test message".to_string(), duration);
        
        if let AppEvent::Status(msg, level, dur) = event {
            assert_eq!(msg, "Test message");
            assert_eq!(level, Level::Info);
            assert_eq!(dur, duration);
        } else {
            panic!("Expected Status event");
        }
    }
}
