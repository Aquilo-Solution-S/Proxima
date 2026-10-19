# Data and erasure
> Status: current.

Proxima supplies inverse operations for its declared storage. The host supplies
authorization policy, retention schedules, obligations, downstream cleanup, and
any record of an operation. The substrate makes no legal-compliance promise.

## Ownership and lifecycle

Every admission belongs to one stable owner. Transfer moves the live admission's
ownership; it does not alter the independent original-owner attribution of a
captured publication. Source attribution selects a source object within one
owner, never a global content hash.

| Operation | Effect |
|---|---|
| Supersede | Append a new version on a series; retain prior versions |
| Forget | Mark an admission forgotten; keep its rows, identity and declared pins |
| Owner erase | Destroy the selected owner's declared storage |
| Source-scope erase | Destroy rows attributable to the selected source within one owner |
| Flavor-scoped erase | Destroy whole series of the flavor's own schemas in one owner |
| Owner export | Return the declared exportable surfaces under host authority |

Forget is not hard deletion. The memory row, its sidecar rows and its sketch
stay in Postgres marked `forgotten_at`; reads hide them, and their search
projection and embeddings are deleted. A forgotten memory still grounds what was
derived from it, so forgetting the last source of an Abstraction or Perspective is
allowed and the derived memory stays readable. A forgotten memory keeps its
series: a later write under the same key, a changed `core_remember` or
`core_derive`, adds a newer version to that series. Replaying the exact write
of a forgotten version answers with that version and `forgotten: true` and
writes nothing.

Restore (`core_restore`) clears the mark: the search
projection is rebuilt in the language the memory was written in, embedding jobs
are queued for the spaces the owner's route names now, and the memory is the
head of its series again unless a newer live version is. Restoring a live
memory changes nothing. Neither forget nor restore touches an object store.

The kernel states `wipeable` and owner-scope principles in
[`Compliance.lean`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/docs/lean/Causa/Compliance.lean).
Rust operations and integration tests define the executable lifecycle behavior.

## Host authority

`OwnerEraseAuthorityPort` answers whether the caller may erase or export a
target owner. With no provider, owner erase and export refuse. The in-process
system path is separately authorized; delegated workers do not acquire owner
erase authority by holding a user's token.

Group-owner operations take the membership lock and recheck that the group has
no members inside the transaction. Personal-owner operations require the host's
`OwnerDropProofPort` to confirm the named drop event. Source-scope operations
keep those preconditions and narrow the selection to attributable rows. Rows
without source attribution are outside a source selection.

Register host-state lifecycle participants at boot when host tables must follow
these operations. The registered participant/table descriptor bounds callbacks;
a callback cannot expand the core selection. See
[host integration](host-integration.md#erase-and-export-callbacks).

## Declared inverses

Each storage surface declares its erase, export, and transfer behavior.
Generated inverse legs follow those declarations. Constraint-backed cascades
are verified against PostgreSQL's catalog; exceptional legs have an explicit
implementation and completeness test. Table definitions belong to migrations;
the typed declarations live beside their storage code.

Erase commits database destruction and external-object purge debt atomically.
Object deletion happens after commit, so a rolled-back erase cannot lose bytes.
Shared objects survive while another admission still claims them; object-key
fences serialize competing claim/delete decisions. An upload owes both of its
keys, the transfer key `pending/<id>` and the canonical `objects/<id>`, whatever
its row records: erase and abort enqueue both, so bytes left by an interrupted
upload or a failed transfer-key delete are reached too. A key that never
existed clears as absent.

Each erased target leaves a payload-free, owner-free identity/kind witness.
Retained source pins stay byte-for-byte; rendering reports a redacted target.
A pin or lineage edge to a memory the reader could read but which is forgotten
renders as `forgotten target` (lineage node status `forgotten`), and lineage
stops there; a reader without the owner or kind permit still sees a redacted
target. Other stores' forgotten memories stay `unavailable`.
New writes must resolve live targets and cannot use these witnesses to recreate
erased identities.

## Publication copies

Outbox records are captured typed delivery copies. Forgetting their Fact cancels
its undelivered delivery: pending and claimed lanes are deleted, and a record
with no delivered lane is removed; a record some consumer already delivered is
kept, settled. A lane already claimed may still deliver once (its mark is then
stale). Restoring the Fact does not re-enqueue. The publication origin stays.
Whole-owner and source erasure remove retained publication
copies by immutable original attribution, including after the live Fact has
transferred. Exact physical Fact erasure removes its publication copy and origin
by admission identity.

Published-body retention is housekeeping. It reaches delivered bodies only; it
does not authorize dropping pending delivery. Original publication attribution
survives body pruning for the inverse path. Neither the delivery copy nor that
internal attribution index is an additional exported memory.

A host intake or broker may already hold bytes. Core cannot retract those bytes;
the host must route deletion and export obligations to those systems itself.

## Flavor-scoped erase and retention

`UnitOfWork::erase_own_series` accepts only the caller flavor's schemas, one
owner, and a typed `SeriesSelection`. Admin or host system authority is required.
From a tool, the dispatched action must declare a destructive effect and belong
to the flavor contract.

Selections can name explicit admissions, admission-time cutoffs, typed sidecar
predicates, or a declared timestamp column. A cutoff selects a whole series only
when its newest version qualifies. Forgotten versions are included when the
series is selected, and they keep their sidecar rows, so a sidecar predicate or
a declared timestamp matches them as it matches live versions. Cross-owner, foreign-schema, or unerasable references refuse before
deletion. Reference closure and transaction bounds are part of the typed API.

The erase is the unit's sole operation. Dry-run uses the same selection and
rollback path. Bounded selections report remaining work; retrying an already
erased series is a no-op. Rustdoc owns selection fields, caps, and receipt types.

The host implements retention by scheduling these selections. Admission time
and declared event time are different clocks: backfilled old events receive a
new admission time. Choose the clock that matches the host's policy, and honor
any hold in the host before calling the inverse.

## Receipts and export

Receipts identify the operation and scope, counts derived from declared
surfaces, and outstanding external purge debt. Core does not persist an erase
journal. A host that needs evidence stores the receipt under its own policy.
An interrupted response may have followed a successful commit; use the
operation's retry contract and inspect outstanding debt.

Owner export returns a deterministic bundle of declared exportable rows. A
count is derived from the exported rows, not a separately maintained total.
Typed content, provenance, and forgotten marks follow the export contracts;
delivery envelopes and rebuildable infrastructure are not independent user
facts. Export authority does not grant destruction authority.

The executable inverse and lifecycle tests cover owner/source isolation,
transfer, rollback, object purge, shared blobs, host-state callbacks, and
publication origins. Operate the external purge lane and verify restored
stores as described in [operate](operate.md).
