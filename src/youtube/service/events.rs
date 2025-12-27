use crate::youtube::ResolvedYouTubeSong;
use anyhow::Result;

/// Event emitter for YouTube service operations
pub trait YouTubeEventEmitter: Send + Sync {
    /// Emit a search result event
    fn emit_search_result(&self, song: ResolvedYouTubeSong, generation: u64) -> Result<()>;

    /// Emit search completion event
    fn emit_search_complete(&self, generation: u64, total_results: usize) -> Result<()>;

    /// Emit error event
    fn emit_error(&self, error: anyhow::Error, operation: &str) -> Result<()>;
}