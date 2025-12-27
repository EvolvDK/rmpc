use crate::{
    shared::events::AppEvent,
    youtube::{
        r#impl::YouTubeServiceImpl,
        security::validate_and_sanitize_query,
        service::{SearchContext, YouTubeService},
    },
};
use anyhow::Result;
use std::sync::Arc;

/// Bridges legacy search calls to the new service-based architecture.
pub async fn migrate_legacy_search(
    service: Arc<YouTubeServiceImpl>,
    query: &str,
    event_tx: crossbeam::channel::Sender<AppEvent>,
) -> Result<()> {
    let sanitized_query = validate_and_sanitize_query(query)?;
    let context = SearchContext { generation: 1, ..Default::default() };
    let emitter = super::CrossbeamEventEmitter::new(event_tx);
    service.search(&sanitized_query, context, &emitter).await.map_err(Into::into)
}
