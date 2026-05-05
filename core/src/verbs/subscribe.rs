//! Subscribe verb — async ChangeEvent stream scoped to one
//! Owner, with optional `since` cursor for resume. See
//! docs/14-protocol-surface.md §"Subscribe".

use std::pin::Pin;

use futures_util::Stream;
use uuid::Uuid;

use crate::{ChangeEvent, Owner};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SubscribeRequest {
    pub owner: Owner,
    /// Resume cursor. Server returns events with `seq > since`.
    /// `None` means "from the beginning of the change log".
    pub since: Option<Uuid>,
}

pub type ChangeEventStream = Pin<Box<dyn Stream<Item = ChangeEvent> + Send>>;
