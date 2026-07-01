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
