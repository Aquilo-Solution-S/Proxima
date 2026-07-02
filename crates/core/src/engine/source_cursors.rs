use super::{Engine, pipeline::WritePermit};
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::{Cursor, Owner};

impl Engine {
    /// Load the owner-scoped opaque cursor for `source`.
    ///
    /// Source cursors are projector write-state, so reads use the same
    /// owner write gate as stores.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot write the owner with
    /// `Ingest`, and `Internal` for storage failures.
    pub async fn load_source_cursor(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        source: &str,
    ) -> Result<Option<Cursor>, ProtocolError> {
        let permit = self.authorize_write(authz, owner, Relation::Ingest).await?;
        self.load_source_cursor_authorized(&permit, source).await
    }

    async fn load_source_cursor_authorized(
        &self,
        permit: &WritePermit,
        source: &str,
    ) -> Result<Option<Cursor>, ProtocolError> {
        self.storage
            .source_cursor
            .source_cursor
            .load_source_cursor(permit.owner(), source)
            .await
            .map_err(|e| ProtocolError::internal(format!("load_source_cursor: {e}")))
    }

    /// Store the owner-scoped opaque cursor for `source`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot write the owner with
    /// `Ingest`, and `Internal` for storage failures.
    pub async fn store_source_cursor(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        source: &str,
        cursor: &Cursor,
    ) -> Result<(), ProtocolError> {
        let permit = self.authorize_write(authz, owner, Relation::Ingest).await?;
        self.store_source_cursor_authorized(&permit, source, cursor)
            .await
    }

    async fn store_source_cursor_authorized(
        &self,
        permit: &WritePermit,
        source: &str,
        cursor: &Cursor,
    ) -> Result<(), ProtocolError> {
        self.storage
            .source_cursor
            .source_cursor
            .store_source_cursor(permit.owner(), source, cursor)
            .await
            .map_err(|e| ProtocolError::internal(format!("store_source_cursor: {e}")))
    }
}
