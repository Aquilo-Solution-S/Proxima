//! EventHistory verb — bounded snapshot read of the change-event
//! log for one Owner. See docs/14 §"EventHistory" and §"Cold-start
//! stitching".

use crate::{ChangeEvent, Owner};

pub const MAX_EVENT_HISTORY_LIMIT: u32 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct EventHistoryRequest {
    pub owner: Owner,
    pub limit: u32,
    pub before: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct EventHistoryResponse {
    pub events: Vec<ChangeEvent>,
    pub seq_high_water: Option<uuid::Uuid>,
}
