// src/youtube/impl/register.rs

use crate::youtube::service::YouTubeService;
use anyhow::{anyhow, Result};
use std::{collections::HashMap, sync::Arc};

/// A register for managing multiple named instances of a YouTube service.
///
/// This allows the application to potentially support different YouTube service
/// configurations or implementations and switch between them.
#[derive(Default)]
pub struct YouTubeServiceRegistry {
    services: HashMap<String, Arc<dyn YouTubeService>>,
    default_service_name: Option<String>,
}

impl YouTubeServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a service instance, optionally setting it as the default.
    ///
    /// If `is_default` is true, this service will become the new default.
    /// If this is the first service being registered, it will automatically
    /// become the default unless a default already exists.
    pub fn register(&mut self, name: String, service: Arc<dyn YouTubeService>, is_default: bool) {
        self.services.insert(name.clone(), service);
        if is_default || self.default_service_name.is_none() {
            self.default_service_name = Some(name);
        }
    }

    /// Gets a service by its name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn YouTubeService>> {
        self.services.get(name).cloned()
    }

    /// Gets the default registered service.
    ///
    /// Returns an error if no default service has been registered.
    pub fn get_default(&self) -> Result<Arc<dyn YouTubeService>> {
        self.default_service_name
            .as_deref()
            .and_then(|name| self.get(name))
            .ok_or_else(|| anyhow!("No default YouTube service has been registered"))
    }
}
