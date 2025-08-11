use crate::mpd::{errors::MpdError, LineHandled, FromMpd};

#[derive(Debug, Default)]
pub struct AddId {
    pub id: Option<u32>,
}

impl FromMpd for AddId {
    fn next_internal(&mut self, key: &str, value: String) -> Result<LineHandled, MpdError> {
        if key == "id" {
            self.id =
                Some(value.parse().map_err(|e| MpdError::Generic(format!("Failed to parse Id: {e}")))?);
            Ok(LineHandled::Yes)
        } else {
            Ok(LineHandled::No { value })
        }
    }
}
