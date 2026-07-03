use crate::Owner;
use crate::access::AccessKind;

/// Sealed owner-write carrier for storage-tier writes.
///
/// Engine authorization is the only constructor. Storage backends use the
/// stamped owner from this permit rather than accepting caller-supplied owner
/// authority.
#[derive(Debug)]
pub struct OwnerWritePermit {
    owner: Owner,
    access_kind: AccessKind,
    _private: (),
}

impl OwnerWritePermit {
    #[must_use]
    pub(crate) const fn new(owner: Owner, access_kind: AccessKind) -> Self {
        Self {
            owner,
            access_kind,
            _private: (),
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

/// Unforgeable witness that engine admission already enforced the relation
/// descriptor's source-owner, owner-policy, and target-access gates before a
/// storage backend performs the atomic edge append.
#[derive(Debug, Clone, Copy)]
pub struct EdgeWriteProof {
    _private: (),
}

impl EdgeWriteProof {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

/// Unforgeable witness that engine admission already authorized the
/// agent-authored derived-memory write (owner write permit, supersedes
/// ownership/kind checks, and edge target-access gates) before a storage
/// backend performs the atomic derive append.
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
