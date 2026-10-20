use super::Engine;
use crate::access::AccessKind;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::verbs::query::{
    ResolvedSidecarHeads, SidecarHeadsPage, SidecarHeadsRequest, SidecarHeadsScope,
};

impl Engine {
    /// List readable current heads of one declared stateful Fact sidecar.
    ///
    /// A key prefix matches complete natural-key columns in declaration
    /// order. Flavor tombstones remain heads; forgotten and superseded
    /// admissions are absent. Pages use descending admission IDs.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for an ineligible schema, an overlong key
    /// prefix, or a zero limit; `Forbidden` when the scope is unreadable;
    /// `Internal` when the backend cannot perform the listing.
    pub async fn sidecar_heads_by_key(
        &self,
        authz: &AuthzContext,
        request: &SidecarHeadsRequest,
    ) -> Result<SidecarHeadsPage, ProtocolError> {
        let query = ResolvedSidecarHeads::resolve(self.registry.contracts(), request)?;
        let read_owners = match &request.scope {
            SidecarHeadsScope::Owner(owner) => vec![
                self.authorize_owner_read(authz, owner, AccessKind::Fact)
                    .await?,
            ],
            SidecarHeadsScope::Readable => self.authorize_read(authz).await?,
        };
        if read_owners.is_empty() {
            return Ok(SidecarHeadsPage::default());
        }
        self.storage
            .query
            .memory_read
            .sidecar_heads_by_key(authz.owner_scope(), &read_owners, &query)
            .await
            .map_err(|error| match error {
                crate::StorageError::ConstraintViolation(message) => {
                    ProtocolError::invalid_argument("key_prefix", message)
                }
                other => super::errors::internal_storage_error("sidecar_heads_by_key", &other),
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::Engine;
    use crate::authz::{AuthPath, AuthorizationHook, AuthzContext, AuthzInput, AuthzVeto};
    use crate::error::ErrorCode;
    use crate::verbs::query::{
        ResolvedSidecarHeads, SidecarAtom, SidecarHeadsRequest, SidecarHeadsScope,
    };
    use crate::{FlavorRegistry, MemoryId, SchemaId, UserId};

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
    }

    fn request() -> SidecarHeadsRequest {
        SidecarHeadsRequest {
            schema_id: SchemaId::new("core/agent-note-v1".into()),
            scope: SidecarHeadsScope::Readable,
            key_prefix: Vec::new(),
            after: None,
            limit: 100,
        }
    }

    #[test]
    fn resolves_identifiers_from_the_frozen_contract_and_bounds_the_page() {
        let engine = engine();
        let mut request = request();
        request
            .key_prefix
            .push(SidecarAtom::Uuid(uuid::Uuid::new_v4()));
        request.after = Some(MemoryId::new(uuid::Uuid::new_v4()));
        request.limit = u32::MAX;
        let query = ResolvedSidecarHeads::resolve(engine.registry.contracts(), &request)
            .expect("stateful core Fact");

        assert_eq!(query.schema_id(), &request.schema_id);
        assert_eq!(query.sidecar_table(), "proxima_core.agent_note_v1");
        assert_eq!(query.memory_key_column(), "t");
        assert_eq!(query.key_columns(), &["note_id"]);
        assert_eq!(query.key_prefix(), request.key_prefix);
        assert_eq!(query.after(), request.after);
        assert_eq!(query.limit(), 1_000);
    }

    #[test]
    fn refuses_zero_limit() {
        let engine = engine();
        let mut request = request();
        request.limit = 0;
        let error = ResolvedSidecarHeads::resolve(engine.registry.contracts(), &request)
            .expect_err("zero is not a page");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains("limit"));
    }

    #[test]
    fn refuses_unknown_or_stateless_schemas() {
        let engine = engine();
        for schema in ["undeclared/note-v1", "core/utterance-v1"] {
            let mut request = request();
            request.schema_id = SchemaId::new(schema.into());
            let error = ResolvedSidecarHeads::resolve(engine.registry.contracts(), &request)
                .expect_err("only a declared stateful Fact has heads by natural key");
            assert_eq!(error.code, ErrorCode::InvalidArgument);
            assert!(error.message.contains("schema_id"));
        }
    }

    #[test]
    fn refuses_prefix_longer_than_the_declared_natural_key() {
        let engine = engine();
        let mut request = request();
        request.key_prefix = vec![SidecarAtom::Bool(true), SidecarAtom::Bool(false)];
        let error = ResolvedSidecarHeads::resolve(engine.registry.contracts(), &request)
            .expect_err("the declaration has one natural-key column");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains("key_prefix"));
    }

    #[tokio::test]
    async fn refuses_an_unreadable_owner_before_storage() {
        let engine = engine();
        let subject = UserId::new(uuid::Uuid::new_v4());
        let authz = AuthzContext::for_subject_with_role(subject, [], AuthPath::HostBearer);
        let mut request = request();
        request.scope = SidecarHeadsScope::Owner(UserId::new(uuid::Uuid::new_v4()).into());
        let error = engine
            .sidecar_heads_by_key(&authz, &request)
            .await
            .expect_err("foreign owner must fail before the unsupported storage backend");
        assert_eq!(error.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn refuses_a_denied_readable_scope_before_storage() {
        let engine = engine();
        let subject = UserId::new(uuid::Uuid::new_v4());
        let authz = AuthzContext::for_subject_with_role(subject, [], AuthPath::Denied);
        let error = engine
            .sidecar_heads_by_key(&authz, &request())
            .await
            .expect_err("denied scope must fail before the unsupported storage backend");
        assert_eq!(error.code, ErrorCode::Forbidden);
    }

    #[derive(Debug)]
    struct Veto;

    impl AuthorizationHook for Veto {
        fn veto(&self, _input: &AuthzInput<'_>) -> Result<(), AuthzVeto> {
            Err(AuthzVeto("test read veto".into()))
        }
    }

    #[tokio::test]
    async fn all_vetoed_readable_owners_return_an_empty_page_before_storage() {
        let mut registry = FlavorRegistry::new();
        registry.add_authorization_hook(Arc::new(Veto));
        let engine = Engine::new(registry.freeze_or_panic_for_tests());
        let subject = UserId::new(uuid::Uuid::new_v4());
        let authz = AuthzContext::for_subject_with_role(subject, [], AuthPath::HostBearer);
        let page = engine
            .sidecar_heads_by_key(&authz, &request())
            .await
            .expect("all-vetoed read set reveals no veto or storage support");
        assert!(page.heads.is_empty());
        assert!(page.next_cursor.is_none());
    }
}
