//! Flavor-shipped personality snapshots for operator fan-out.

use async_trait::async_trait;

use crate::{Owner, error::ProtocolError, operators::PersonalitySnapshot};

/// Read-only handle passed to `PersonalityFlavor::snapshot`.
///
/// v1 exposes only the owner. Later P_active / G_active query helpers
/// can land here without changing the operator contract.
#[derive(Debug)]
pub struct PersonalityContext<'a> {
    pub owner: &'a Owner,
}

/// Build-time personality contribution from a flavor.
#[async_trait]
pub trait PersonalityFlavor: Send + Sync + std::fmt::Debug {
    fn personality_id(&self) -> &'static str;

    async fn snapshot(
        &self,
        ctx: &PersonalityContext<'_>,
    ) -> Result<PersonalitySnapshot, ProtocolError>;
}
