use thiserror::Error;

#[derive(Debug, Error)]
pub enum DataStoreError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
}

pub struct DataStore;

impl DataStore {
    pub fn new() -> Result<Self, DataStoreError> {
        unimplemented!()
    }
}
