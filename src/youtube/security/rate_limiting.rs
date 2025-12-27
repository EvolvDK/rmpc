use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static GLOBAL_RATE_LIMITERS: Lazy<DashMap<String, Mutex<RateLimiter>>> = Lazy::new(DashMap::new);

#[derive(Debug, Clone)]
pub struct RateLimiterConfig {
    pub requests: usize,
    pub window: Duration,
}

#[derive(Debug, Clone)]
pub struct RateLimiter {
    requests: VecDeque<Instant>,
    window_size: Duration,
    max_requests: usize,
}

impl RateLimiter {
    /// Creates a new rate limiter with a given configuration.
    pub fn new(max_requests: usize, window_size: Duration) -> Self {
        Self {
            requests: VecDeque::with_capacity(max_requests),
            window_size,
            max_requests,
        }
    }
    
    /// Checks if a request is allowed. If it is, the request is recorded.
    /// Returns `true` if the request is allowed, `false` otherwise.
    pub fn check(&mut self) -> bool {
        let now = Instant::now();
        self.requests.retain(|&timestamp| now.duration_since(timestamp) < self.window_size);

        if self.requests.len() < self.max_requests {
            self.requests.push_back(now);
            true // Request is allowed
        } else {
            false // Rate limit exceeded
        }
    }
}

/// Checks and updates a globally-managed rate limiter.
///
/// # Arguments
/// * `key` - A unique identifier for the rate limit being checked (e.g., an API key or service name).
/// * `config` - A vector of rate limit configurations (e.g., per minute, per hour) to apply.
///
/// # Returns
/// `true` if all rate limits are respected, `false` otherwise.
pub fn check_global_rate_limit(key: &str, configs: &[RateLimiterConfig]) -> bool {
    for config in configs {
        // Construct a unique key for each time window (e.g., "default_api_60s").
        let specific_key = format!("{}_{}s", key, config.window.as_secs());
        
        // Get or create the rate limiter for this specific key.
        let limiter_entry = GLOBAL_RATE_LIMITERS
            .entry(specific_key)
            .or_insert_with(|| Mutex::new(RateLimiter::new(config.requests, config.window)));
        
        // Lock and check. If this check fails, return false immediately.
        let mut limiter = limiter_entry.lock().unwrap();
        if !limiter.check() {
            log::warn!("Global rate limit exceeded for key '{}' ({} req / {:?})", key, config.requests, config.window);
            return false;
        }
    }
    // All configured rate limits passed.
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_rate_limiter_allows_up_to_limit() {
        let mut limiter = RateLimiter::new(3, Duration::from_secs(1));
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(!limiter.check()); // Fourth request should be denied
    }

    #[test]
    fn test_rate_limiter_resets_after_window() {
        let mut limiter = RateLimiter::new(2, Duration::from_millis(100));
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(!limiter.check());

        thread::sleep(Duration::from_millis(110));

        assert!(limiter.check()); // Window has passed, new request is allowed
    }
    
    #[test]
    fn test_global_rate_limiter_enforces_multiple_configs() {
        // Config: 2 requests per second AND 5 requests per 5 seconds
        let configs = vec![
            RateLimiterConfig { requests: 2, window: Duration::from_secs(1) },
            RateLimiterConfig { requests: 5, window: Duration::from_secs(5) },
        ];
        let key = "test_key_global";

        // First 2 should pass
        assert!(check_global_rate_limit(key, &configs));
        assert!(check_global_rate_limit(key, &configs));

        // 3rd should fail the 1-second limit
        assert!(!check_global_rate_limit(key, &configs));
        
        // Wait for the 1-second window to pass
        thread::sleep(Duration::from_secs(1));

        // 3rd, 4th, 5th should pass now
        assert!(check_global_rate_limit(key, &configs));
        assert!(check_global_rate_limit(key, &configs));
        assert!(check_global_rate_limit(key, &configs));
        
        // 6th should fail the 5-second limit
        assert!(!check_global_rate_limit(key, &configs));
    }
}
