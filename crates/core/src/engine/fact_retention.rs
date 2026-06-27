use super::{Engine, MemoryPermit};
use crate::MemoryId;
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::owner::Owner;
use crate::sidecar_tables;
use crate::verbs::fact_cleanup::{CleanupDueFactsOutcome, TombstoneFactOutcome};
use crate::verbs::schema::PayloadKind;

fn retention_seconds_to_i64(seconds: u64) -> Result<i64, ProtocolError> {
    if seconds == 0 {
        return Err(ProtocolError::invalid_argument(
            "retention_seconds",
            "must be greater than 0",
        ));
    }
    i64::try_from(seconds).map_err(|_| {
        ProtocolError::invalid_argument("retention_seconds", "must fit in signed 64-bit integer")
    })
}

impl Engine {
    /// Set the owner-scoped Fact retention duration.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `owner` or
    /// lacks `Admin`, `InvalidArgument` for a zero/out-of-range duration,
    /// and `Internal` for storage failures.
    pub async fn set_fact_retention(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        seconds: u64,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .authorize_request(authz, owner, Relation::Admin)
            .await?;
        self.set_fact_retention_authorized(&permit, owner, seconds)
            .await
    }

    async fn set_fact_retention_authorized(
        &self,
        permit: &MemoryPermit,
        _owner: &Owner,
        seconds: u64,
    ) -> Result<(), ProtocolError> {
        let seconds = retention_seconds_to_i64(seconds)?;
        self.storage
            .upsert_fact_retention(permit.owner(), seconds)
            .await
            .map_err(|e| ProtocolError::internal(format!("set_fact_retention: {e}")))
    }

    /// Read the owner-scoped Fact retention duration.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `owner` or
    /// lacks `Admin`, and `Internal` for storage failures.
    pub async fn get_fact_retention(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
    ) -> Result<Option<i64>, ProtocolError> {
        let permit = self
            .authorize_request(authz, owner, Relation::Admin)
            .await?;
        self.get_fact_retention_authorized(&permit, owner).await
    }

    async fn get_fact_retention_authorized(
        &self,
        permit: &MemoryPermit,
        _owner: &Owner,
    ) -> Result<Option<i64>, ProtocolError> {
        self.storage
            .get_fact_retention(permit.owner())
            .await
            .map_err(|e| ProtocolError::internal(format!("get_fact_retention: {e}")))
    }

    /// Clear the owner-scoped Fact retention duration.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `owner` or
    /// lacks `Admin`, and `Internal` for storage failures.
    pub async fn clear_fact_retention(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
    ) -> Result<bool, ProtocolError> {
        let permit = self
            .authorize_request(authz, owner, Relation::Admin)
            .await?;
        self.clear_fact_retention_authorized(&permit, owner).await
    }

    async fn clear_fact_retention_authorized(
        &self,
        permit: &MemoryPermit,
        _owner: &Owner,
    ) -> Result<bool, ProtocolError> {
        self.storage
            .clear_fact_retention(permit.owner())
            .await
            .map_err(|e| ProtocolError::internal(format!("clear_fact_retention: {e}")))
    }

    /// Hard-erase due Facts, tombstone their transitive derived
    /// memory dependents, and erase orphaned citation backing rows.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `owner` or
    /// lacks `Admin`, and `Internal` for storage failures.
    pub async fn cleanup_due_facts(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
    ) -> Result<CleanupDueFactsOutcome, ProtocolError> {
        let permit = self
            .authorize_request(authz, owner, Relation::Admin)
            .await?;
        self.cleanup_due_facts_authorized(&permit, owner).await
    }

    async fn cleanup_due_facts_authorized(
        &self,
        permit: &MemoryPermit,
        _owner: &Owner,
    ) -> Result<CleanupDueFactsOutcome, ProtocolError> {
        let fact_sidecar_tables = sidecar_tables(self.registry.schemas(), PayloadKind::Fact);
        let edge_sidecar_tables = sidecar_tables(self.registry.schemas(), PayloadKind::Edge);
        let citation_mapping_sidecar_tables =
            sidecar_tables(self.registry.schemas(), PayloadKind::CitationMapping);
        let cited_object_sidecar_tables =
            sidecar_tables(self.registry.schemas(), PayloadKind::CitedObject);
        self.storage
            .cleanup_due_facts(
                permit.owner(),
                &fact_sidecar_tables,
                &edge_sidecar_tables,
                &citation_mapping_sidecar_tables,
                &cited_object_sidecar_tables,
            )
            .await
            .map_err(|e| ProtocolError::internal(format!("cleanup_due_facts: {e}")))
    }

    /// Hard-erase one Fact, cascade soft-tombstones through all transitive
    /// Provenance derivatives, and erase orphaned citation backing rows.
    /// Gated by `SourceIngest` because targeted forgetting is symmetric with
    /// Fact creation by `EventIngest`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `owner` or lacks
    /// [`Relation::Ingest`] on the owner space, and `Internal` for storage
    /// failures.
    pub async fn tombstone_fact(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        fact_id: MemoryId,
    ) -> Result<TombstoneFactOutcome, ProtocolError> {
        let permit = self
            .authorize_request(authz, owner, Relation::Ingest)
            .await?;
        let fact_sidecar_tables = sidecar_tables(self.registry.schemas(), PayloadKind::Fact);
        let edge_sidecar_tables = sidecar_tables(self.registry.schemas(), PayloadKind::Edge);
        let citation_mapping_sidecar_tables =
            sidecar_tables(self.registry.schemas(), PayloadKind::CitationMapping);
        let cited_object_sidecar_tables =
            sidecar_tables(self.registry.schemas(), PayloadKind::CitedObject);
        self.storage
            .tombstone_fact(
                permit.owner(),
                fact_id.into_inner(),
                &fact_sidecar_tables,
                &edge_sidecar_tables,
                &citation_mapping_sidecar_tables,
                &cited_object_sidecar_tables,
            )
            .await
            .map_err(|e| ProtocolError::internal(format!("tombstone_fact: {e}")))
    }
}
