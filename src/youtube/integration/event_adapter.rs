use crate::{
    shared::events::{AppEvent, WorkDone},
    youtube::{service::YouTubeEventEmitter, ResolvedYouTubeSong},
};
use anyhow::{Context, Result};
use crossbeam::channel::Sender;

pub struct CrossbeamEventEmitter {
    sender: Sender<AppEvent>,
}

impl CrossbeamEventEmitter {
    pub fn new(sender: Sender<AppEvent>) -> Self {
        Self { sender }
    }
}

impl YouTubeEventEmitter for CrossbeamEventEmitter {
    fn emit_search_result(&self, song: ResolvedYouTubeSong, generation: u64) -> Result<()> {
        let event = AppEvent::WorkDone(Ok(WorkDone::YouTubeSearchResult {
            song_info: song,
            generation,
        }));
        self.sender.send(event).context("Failed to send event")
    }

    fn emit_search_complete(&self, generation: u64, _total_results: usize) -> Result<()> {
        let event = AppEvent::WorkDone(Ok(WorkDone::YouTubeSearchFinished { generation }));
        self.sender.send(event).context("Failed to send event")
    }

    fn emit_error(&self, error: anyhow::Error, _operation: &str) -> Result<()> {
        self.sender
            .send(AppEvent::WorkDone(Err(error)))
            .context("Failed to send event")
    }
}
