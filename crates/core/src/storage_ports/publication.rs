//! Host-only drain port for the Fact publication outbox (issue #305).
//!
//! Deliberately NOT part of [`crate::storage_ports::StoragePorts`]: the
//! engine never holds it, `ToolCtx`, `WriteSession` and `UnitOfWork` never
//! see it, and no flavor-facing facade exposes it. The storage BACKEND type
//! implements it and the host runtime holds that handle, which is the only
//! shape in which "claim other owners' rows and ship them to a broker" is a
//! legitimate operation: a publisher is infrastructure, not a caller.
//!
//! [`PublicationOutboxPort`] has no delete or purge method, and never
//! should: an expiring lease can only make a record deliverable again, never
//! destroy it. Housekeeping of records that ARE delivered lives on a second,
//! separate host-only trait, [`PublicationRetentionPort`], so that "a
//! publisher may drain" and "an operator may reclaim delivered storage" stay
//! two capabilities rather than one.

use std::num::NonZeroU32;
use std::time::Duration;

use uuid::Uuid;

use crate::MemoryId;
use crate::owner::{OwnerRef, OwnerRefKind};
use crate::storage::StorageError;

/// Fail-closed result of checking whether one exact captured Fact may still
/// be admitted by a host consumer. This reveals no payload or foreign owner
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationOriginEligibility {
    Eligible,
    Ineligible,
}

/// Narrow host-only check over immutable publication origin and the core
/// hard-delete witness. It is deliberately separate from `StoragePorts` and
/// grants neither Fact reads nor writes.
#[async_trait::async_trait]
pub trait PublicationOriginEligibilityPort: Send + Sync {
    /// Check one exact `(original owner, physical Fact)` pair in its own
    /// short transaction. This method takes the database-wide shared
    /// lifecycle fence before the existing per-Fact target fence, commits,
    /// and only then returns the result to a host caller. It grants no
    /// authority to read the Fact or retain a database fence across an
    /// external operation.
    ///
    /// This convenience is for host work that must not carry a caller's
    /// transaction into an external await. Atomic host-state admission must
    /// continue to use [`Self::check_in_transaction`].
    ///
    /// # Errors
    /// Returns a storage error when the fenced check or commit fails.
    async fn check_committed(
        &self,
        original_owner: OwnerRef,
        fact_id: MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError>;

    /// Check one `(original owner, physical Fact)` pair inside the caller's
    /// current `UoW` transaction, under the same target fence used by hard
    /// deletion. The caller must hold the database-wide lifecycle fence in
    /// shared mode from transaction entry, before acquiring owner or target
    /// locks; this method does not acquire that fence after the fact. The
    /// caller must commit any corresponding host-state write in this
    /// transaction; a pool-only preflight is not authoritative.
    ///
    /// # Errors
    /// Returns a storage error when the fenced check cannot be completed.
    async fn check_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        original_owner: OwnerRef,
        fact_id: MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError>;
}

/// Identity of one publisher process, recorded on every claim.
///
/// Operator-facing, not a credential: the fencing token is
/// [`ClaimToken`]. This answers "who is holding this record right now" in an
/// operator's incident, which a UUID alone does not.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublisherId(String);

impl PublisherId {
    /// # Errors
    ///
    /// Returns [`PublisherIdError`] for an empty or whitespace-only name, a
    /// name carrying control characters, or one longer than 128 bytes.
    pub fn new(raw: impl Into<String>) -> Result<Self, PublisherIdError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(PublisherIdError::Empty);
        }
        if raw.chars().any(char::is_control) {
            return Err(PublisherIdError::IllegalCharacter { value: raw });
        }
        if raw.len() > 128 {
            return Err(PublisherIdError::TooLong { bytes: raw.len() });
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PublisherId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublisherIdError {
    #[error("publisher id must not be empty")]
    Empty,
    #[error("publisher id {value:?} must not carry control characters")]
    IllegalCharacter { value: String },
    #[error("publisher id is {bytes} bytes, over the 128-byte limit")]
    TooLong { bytes: usize },
}

/// The identity of one Proxima installation, minted once by the schema.
///
/// This exists because [`PublicationOriginEligibility`] is answered from the
/// ABSENCE of a row, and an absence carries no scope. "No origin row for
/// this Fact" reads identically whether the row was revoked by erasure or
/// was never in this database at all, so a cleaner pointed at a stream some
/// other installation published to would read every message as revoked.
///
/// A publisher stamps this on the broker message; a cleaner compares it
/// against the value it read from the same database that answers the
/// eligibility check. That makes the binding a fact about the data rather
/// than a deployment precondition an operator is trusted to have met.
///
/// It identifies an installation LINEAGE, not a database instance: a
/// restore or a clone carries the same value, which is why pointing a copy
/// at the original's broker remains an operator responsibility (docs/18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OriginScope(Uuid);

impl OriginScope {
    #[must_use]
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

impl std::fmt::Display for OriginScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.as_hyphenated())
    }
}

/// The fencing token minted by one claim.
///
/// A publisher that lost its lease and comes back holds a stale token; the
/// acknowledgement it sends is refused rather than applied, which is what
/// keeps a slow publisher from marking another publisher's delivery as its
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClaimToken(Uuid);

impl ClaimToken {
    #[must_use]
    pub const fn new(token: Uuid) -> Self {
        Self(token)
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

impl std::fmt::Display for ClaimToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One captured event, leased to the claiming publisher.
///
/// `envelope` is the bytes as captured. The publisher ships them unchanged:
/// it does not reload the Fact, re-render the payload with today's flavor,
/// or substitute today's installation identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedPublication {
    /// The Fact's `t`.
    pub id: Uuid,
    /// `CloudEvents` `id` (`F:<uuid>`), and the broker dedup key.
    pub event_id: String,
    pub owner_id: Uuid,
    pub owner_kind: OwnerRefKind,
    pub schema_id: String,
    pub schema_version: u32,
    pub event_type: String,
    pub envelope: Vec<u8>,
    pub digest: [u8; 32],
    /// How many times this record has been claimed, including this one.
    pub attempts: u32,
    pub claim: ClaimToken,
    pub lease_expires_at: time::OffsetDateTime,
}

/// What the broker said about one accepted publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerReceipt {
    pub stream: String,
    pub sequence: u64,
}

/// Outcome of acknowledging a publish.
///
/// A stale worker is never an error: it lost a race it could not have seen,
/// and the record is already someone else's or already done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckOutcome {
    /// This claim marked the record published.
    Published,
    /// The record was already published, by this or another publisher.
    AlreadyPublished,
    /// The claim token no longer holds the record.
    StaleClaim,
}

/// Outcome of releasing a claim back to `pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOutcome {
    Released,
    AlreadyPublished,
    StaleClaim,
}

/// The bounded claim/acknowledge/release surface a shared publisher needs,
/// and nothing else.
#[async_trait::async_trait]
pub trait PublicationOutboxPort: Send + Sync {
    /// Claim up to `limit` deliverable records under a `lease`.
    ///
    /// Deliverable means `pending`, or `claimed` with an expired lease. The
    /// scan carries NO cursor and NO watermark: a transaction that commits
    /// late is simply `pending` on the next call rather than invisible
    /// behind an advanced high-water mark.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the claim statement.
    async fn claim(
        &self,
        publisher: &PublisherId,
        limit: NonZeroU32,
        lease: Duration,
    ) -> Result<Vec<ClaimedPublication>, StorageError>;

    /// Record a successful broker acknowledgement, fenced on `claim`.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the acknowledgement statement.
    async fn mark_published(
        &self,
        id: Uuid,
        claim: ClaimToken,
        receipt: &BrokerReceipt,
    ) -> Result<AckOutcome, StorageError>;

    /// Return a claimed record to `pending`, fenced on `claim`.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the release statement.
    async fn release(&self, id: Uuid, claim: ClaimToken) -> Result<ReleaseOutcome, StorageError>;

    /// Records that are not published yet, claimed or otherwise.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the count query.
    async fn pending_count(&self) -> Result<u64, StorageError>;
}

/// Operator-only housekeeping of records that were already delivered.
///
/// Separate from [`PublicationOutboxPort`] on purpose. A publisher needs
/// claim/ack/release and nothing else; retention needs a DELETE and nothing
/// else. Holding them apart means the handle a drain loop carries cannot
/// remove a record at all, which is the property the outbox rests on: an
/// undelivered event leaves this table only through compliance erasure.
///
/// The implementation is bound by the same rule the trait states — only
/// `published` rows, only ones whose publication is older than the horizon,
/// never a `pending` or `claimed` row whatever its age. A record still
/// waiting for a broker is a promise this deployment has not kept yet, and
/// no retention policy may quietly cancel it.
#[async_trait::async_trait]
pub trait PublicationRetentionPort: Send + Sync {
    /// Delete up to `limit` records that were published longer than
    /// `older_than` ago, and return how many were removed.
    ///
    /// Bounded by `limit` because this runs beside a live write path: an
    /// unbounded DELETE over a large backlog would hold row locks for as
    /// long as it takes, against a table every listenable write inserts
    /// into. A caller that wants more calls again.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the delete statement.
    async fn prune_published(
        &self,
        older_than: Duration,
        limit: NonZeroU32,
    ) -> Result<u64, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_publisher_id_is_a_validated_label() {
        assert_eq!(
            PublisherId::new("proxima-mcp@host-1")
                .expect("valid")
                .as_str(),
            "proxima-mcp@host-1"
        );
        assert!(matches!(
            PublisherId::new("   "),
            Err(PublisherIdError::Empty)
        ));
        assert!(matches!(
            PublisherId::new("bad\nname"),
            Err(PublisherIdError::IllegalCharacter { .. })
        ));
        assert!(matches!(
            PublisherId::new("x".repeat(129)),
            Err(PublisherIdError::TooLong { bytes: 129 })
        ));
    }
}
