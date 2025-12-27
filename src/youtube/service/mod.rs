// src/youtube/service/mod.rs

// --- Declare child modules ---
pub mod config;
pub mod events;
pub mod interfaces;

// --- Re-export all public items from child modules ---
pub use config::*;
pub use events::*;
pub use interfaces::*;
