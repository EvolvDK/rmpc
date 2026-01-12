use crate::youtube::events::YouTubeEvent;
use crate::youtube::models::FilterMode;
use crate::youtube::ytdlp::YouTubeClient;
use async_channel::Sender;
use futures_lite::StreamExt;
use std::sync::Arc;

use parking_lot::Mutex;

pub struct SearchService<C: YouTubeClient> {
    event_tx: Sender<YouTubeEvent>,
    client: Arc<C>,
    active_search: Arc<Mutex<Option<smol::Task<()>>>>,
}

impl<C: YouTubeClient + 'static> SearchService<C> {
    pub fn new(event_tx: Sender<YouTubeEvent>, client: Arc<C>) -> Self {
        Self { event_tx, client, active_search: Arc::new(Mutex::new(None)) }
    }

    pub async fn handle_event(&self, event: YouTubeEvent) {
        match event {
            YouTubeEvent::SearchRequest { query, filter_mode, limit } => {
                let task = self.active_search.lock().take();
                if let Some(task) = task {
                    task.cancel().await;
                }

                let client = self.client.clone();
                let event_tx = self.event_tx.clone();
                let active_search_clone = self.active_search.clone();
                let task = smol::spawn(async move {
                    Self::handle_search_request(client, event_tx, query, filter_mode, limit).await;
                    active_search_clone.lock().take();
                });
                *self.active_search.lock() = Some(task);
            }
            YouTubeEvent::CancelSearch => {
                let task = self.active_search.lock().take();
                if let Some(task) = task {
                    task.cancel().await;
                }
            }
            _ => {}
        }
    }

    async fn handle_search_request(
        client: Arc<C>,
        event_tx: Sender<YouTubeEvent>,
        query: String,
        _filter_mode: FilterMode,
        limit: usize,
    ) {
        let _ = event_tx.send(YouTubeEvent::SearchStarted).await;

        match client.search(&query, limit) {
            Ok(mut stream) => {
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(search_result) => {
                            let _ = event_tx.send(YouTubeEvent::SearchResult(search_result)).await;
                        }
                        Err(e) => {
                            log::error!("Search result error: {e}");
                        }
                    }
                }
                let _ = event_tx.send(YouTubeEvent::SearchFinished).await;
            }
            Err(e) => {
                let _ = event_tx.send(YouTubeEvent::SearchError(e.to_string())).await;
            }
        }
    }
}
