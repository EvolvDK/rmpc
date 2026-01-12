use std::time::Duration;

pub const METADATA_REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 3600);
pub const STARTUP_DELAY: Duration = Duration::from_secs(60);
pub const URL_EXPIRY_BUFFER: u64 = 60; // seconds

pub const YOUTUBE_ID_FRAGMENT: &str = "#id=";
pub const EXPIRE_PARAM: &str = "expire=";

pub const CONCURRENT_YTDLP_LIMIT: usize = 3;
pub const METADATA_CACHE_SIZE: usize = 100;
pub const URL_CACHE_SIZE: usize = 50;
pub const DATABASE_BATCH_SIZE: usize = 50;

pub const DEFAULT_LYRICS_SEARCH_LIMIT: usize = 10;
pub const LYRICS_DURATION_MATCH_THRESHOLD: f64 = 10.0;

pub const STALE_METADATA_THRESHOLD_DAYS: i64 = 7;
pub const METADATA_REFRESH_THROTTLE: Duration = Duration::from_secs(2);
pub const METADATA_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(24 * 3600);
