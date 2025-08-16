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
    core::data_store::models::YouTubeVideo,
    mpd::commands::IdleEvent,
    ui::UiAppEvent,
};

#[derive(Debug, Clone)]
pub(crate) struct RefreshContext {
    pub old_song_id: u32,
    pub position: usize,
    pub play_after_refresh: bool,
}

#[derive(Debug)]
#[allow(unused)]
pub(crate) enum ClientRequest {
    Query(MpdQuery),
    QuerySync(MpdQuerySync),
    Command(MpdCommand),
}

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
        video: YouTubeVideo,
        context: Option<RefreshContext>,
    },
    RefreshYouTubeStream {
        old_song_id: u32,
        position: u32,
        youtube_id: String,
        video_title: String,
    },
    YouTubeGetVideoInfo {
        id: String,
    },
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // the instances are short lived events, its fine.
pub(crate) enum WorkDone {
    LyricsIndexed { index: LrcIndex },
    SingleLrcIndexed { lrc_entry: Option<LrcIndexEntry> },
    YouTubeSearchResult {
        video: YouTubeVideo,
        generation: u64,
    },
    YouTubeSearchFinished {
        generation: u64,
    },
    YouTubeStreamUrlReady {
        url: String,
        video: YouTubeVideo,
        context: Option<RefreshContext>,
    },
    YouTubeStreamUrlFailed {
        video: YouTubeVideo,
        context: Option<RefreshContext>,
    },
    YouTubeStreamRefreshed {
        new_url: String,
        video: YouTubeVideo,
        old_song_id: u32,
        position: u32,
    },
    YouTubeStreamRefreshFailed {
        video_title: String,
    },
    MpdCommandFinished { id: &'static str, target: Option<PaneType>, data: MpdQueryResult },
    YouTubeVideoInfoFetched(YouTubeVideo),
    None,
}

// The instances are short lived events, boxing would most likely only hurt
// here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum AppEvent {
    UserKeyInput(KeyEvent),
    UserMouseInput(MouseEvent),
    Status(String, Level, Duration),
    InfoModal {
        message: Vec<String>,
        title: Option<String>,
        size: Option<Size>,
        replacement_id: Option<String>,
    },
    Log(Vec<u8>),
    IdleEvent(IdleEvent),
    RequestRender,
    Resized {
        columns: u16,
        rows: u16,
    },
    ResizedDebounced {
        columns: u16,
        rows: u16,
    },
    WorkDone(Result<WorkDone>),
    UiEvent(UiAppEvent),
    Reconnected,
    LostConnection,
    TmuxHook {
        hook: String,
    },
    ConfigChanged {
        config: Box<Config>,
        keep_old_theme: bool,
    },
    ThemeChanged {
        theme: Box<UiConfig>,
    },
    RemoteSwitchTab {
        tab_name: String,
    },
    IpcQuery {
        stream: IpcStream,
        targets: Vec<RemoteCommandQuery>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Copy, Eq, Hash, PartialEq)]
#[allow(dead_code)]
pub enum Level {
    Trace,
    Debug,
    Warn,
    Error,
    Info,
}
