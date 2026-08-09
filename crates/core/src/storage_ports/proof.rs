use std::time::SystemTime;

use crate::Owner;
use crate::access::AccessKind;
use crate::authz::DelegationRuntimeBinding;
use crate::error::ProtocolError;

struct DelegatedWriteGuard {
    runtime_binding: DelegationRuntimeBinding,
    expires_at: SystemTime,
}

/// Sealed owner-write carrier for storage-tier writes.
///
/// Engine authorization is the only constructor. Storage backends use the
/// stamped owner from this permit rather than accepting caller-supplied owner
/// authority.
pub struct OwnerWritePermit {
    owner: Owner,
    access_kind: AccessKind,
    delegated: Option<DelegatedWriteGuard>,
    _private: (),
}

impl std::fmt::Debug for OwnerWritePermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("OwnerWritePermit");
        debug
            .field("owner", &self.owner)
            .field("access_kind", &self.access_kind)
            .field("delegated", &self.delegated.is_some());
        if let Some(delegated) = &self.delegated {
            debug.field("expires_at", &delegated.expires_at);
        }
        debug.finish_non_exhaustive()
    }
}

impl OwnerWritePermit {
    #[must_use]
    pub(crate) const fn new(owner: Owner, access_kind: AccessKind) -> Self {
        Self {
            owner,
            access_kind,
            delegated: None,
            _private: (),
        }
    }

    #[must_use]
    pub(crate) fn new_delegated(
        owner: Owner,
        access_kind: AccessKind,
        runtime_binding: DelegationRuntimeBinding,
        expires_at: SystemTime,
    ) -> Self {
        Self {
            owner,
            access_kind,
            delegated: Some(DelegatedWriteGuard {
                runtime_binding,
                expires_at,
            }),
            _private: (),
        }
    }

    pub(crate) fn validate_for_engine(
        &self,
        runtime_binding: &DelegationRuntimeBinding,
    ) -> Result<(), ProtocolError> {
        let Some(delegated) = &self.delegated else {
            return Ok(());
        };
        if delegated.runtime_binding != *runtime_binding {
            return Err(ProtocolError::forbidden(
                "delegated write witness belongs to a different runtime",
            ));
        }
        if delegated.expires_at <= SystemTime::now() {
            return Err(ProtocolError::forbidden(
                "delegated write witness has expired",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn expire_delegated_for_test(&mut self) {
        if let Some(delegated) = &mut self.delegated {
            delegated.expires_at = SystemTime::UNIX_EPOCH;
        }
    }

    #[must_use]
    pub const fn owner(&self) -> &Owner {
        &self.owner
    }

    #[must_use]
    pub const fn access_kind(&self) -> AccessKind {
        self.access_kind
    }
}

/// Unforgeable witness that engine admission already authorized the
/// agent-authored derived-memory write — owner write permit, supersedes
/// ownership/kind checks, and read authority on every declared origin and
/// reference target — before a storage backend performs the atomic derive
/// append.
#[derive(Debug, Clone, Copy)]
pub struct OperatorWriteProof {
    _private: (),
}

impl OperatorWriteProof {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

/// Unforgeable witness that engine admission already authorized the
/// memory/fact write or embedding-job claim that made an entity eligible
/// for an embedding write before a storage backend performs it.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddingWriteProof {
    _private: (),
}

impl EmbeddingWriteProof {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

/// Unforgeable witness that the engine already authorized an owner-agnostic
/// operator maintenance action.
#[derive(Debug, Clone, Copy)]
pub struct OperatorMaintenanceProof {
    _private: (),
}

impl OperatorMaintenanceProof {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}
