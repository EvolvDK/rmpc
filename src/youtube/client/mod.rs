// src/youtube/client/mod.rs

pub mod mock_client;
pub mod ytdlp_client;

pub use mock_client::MockYouTubeClient;
pub use ytdlp_client::{YtDlpClient, YtDlpClientConfig};
