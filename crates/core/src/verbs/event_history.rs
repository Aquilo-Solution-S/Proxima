//! `EventHistory` verb — bounded snapshot read of the change-event
//! log for one Owner. See docs/14 §"`EventHistory`" and §"Cold-start
//! stitching".

use crate::{ChangeEvent, Principal};

pub const MAX_EVENT_HISTORY_LIMIT: u32 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EventHistoryRequest {
    pub principal: Principal,
    pub limit: u32,
    pub before: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EventHistoryResponse {
    pub events: Vec<ChangeEvent>,
    pub seq_high_water: Option<uuid::Uuid>,
}
