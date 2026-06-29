//! `ChangeHistory` verb — bounded snapshot read of visible change-event
//! rows for the authenticated read-owner set. See docs/14 §"`ChangeHistory`"
//! and §"Cold-start stitching".

use crate::{ChangeEvent, OwnerRef};

pub const MAX_CHANGE_HISTORY_LIMIT: u32 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeHistoryRequest {
    pub principal: OwnerRef,
    pub limit: u32,
    pub before: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeHistoryResponse {
    pub events: Vec<ChangeEvent>,
    pub seq_high_water: Option<uuid::Uuid>,
}
