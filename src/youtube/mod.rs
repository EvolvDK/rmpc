// src/youtube/mod.rs

// --- Declare Modules ---
pub mod cache;
pub mod client;
pub mod controllers;
pub mod error;
pub mod r#impl; // Renamed to avoid keyword conflict
pub mod integration;
pub mod security;
pub mod service;
pub mod types;
mod utils;

// --- Public API Re-exports (Prelude) ---

// Re-export controller types for UI consumption.
pub use controllers::{YouTubeLibraryController, YouTubeSearchController};

// Re-export the primary service interface and its factory.
pub use r#impl::factory::YouTubeServiceFactory;
pub use r#impl::register::YouTubeServiceRegistry;
pub use service::YouTubeService;

// Re-export core types used frequently by consumers of this module.
pub use types::{CacheStats, ResolvedYouTubeSong};

// Re-export the public interfaces for the client and cache for dependency injection.
pub use service::{CacheService, YouTubeClient};

// Internal utility function, not part of the public API but used by submodules.
use utils::parse_song_info_json;

// Helper function for appending YouTube ID to a URL, used in UI layer.
pub fn append_youtube_id_to_url(url: String, youtube_id: &str) -> String {
    format!("{}#{}", url, youtube_id)
}
