use crate::youtube::constants::EXPIRE_PARAM;
use crate::youtube::constants::URL_EXPIRY_BUFFER;
use lru::LruCache;
use std::hash::Hash;
use std::num::NonZeroUsize;

pub struct ExpiringCache<K, V> {
    cache: LruCache<K, V>,
}

impl<K: Hash + Eq, V: AsRef<str>> ExpiringCache<K, V> {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self { cache: LruCache::new(capacity) }
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let expired = if let Some(value) = self.cache.peek(key) {
            is_youtube_url_expired(value.as_ref())
        } else {
            false
        };

        if expired {
            self.cache.pop(key);
            return None;
        }

        self.cache.get(key)
    }

    pub fn put(&mut self, key: K, value: V) {
        self.cache.put(key, value);
    }

    pub fn pop(&mut self, key: &K) -> Option<V> {
        self.cache.pop(key)
    }
}

pub fn get_youtube_expiry(url: &str) -> Option<u64> {
    let start = url.find(EXPIRE_PARAM)? + EXPIRE_PARAM.len();
    let end = url[start..].find('&').map_or(url.len(), |pos| start + pos);
    url[start..end].parse().ok()
}

pub fn is_youtube_url_expired(url: &str) -> bool {
    if let Some(expiry) = get_youtube_expiry(url) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Consider it expired if it already expired or expires in less than buffer
        expiry < now + URL_EXPIRY_BUFFER
    } else {
        false
    }
}
