# 19 — Host-state maintenance authority

> **Status:** current.

Decision accepted by Heinrich on 2026-09-19 for Aquilo #8719/#8722: a host
capability may maintain its registered Goal Trigger participant's state for
all personal and Group owners. The kernel contract is implemented in
[`HostStateMaintenance.lean`](lean/Causa/HostStateMaintenance.lean), and the
Rust core, Postgres adapter and embedded-host boundary are implemented in the
current slice. Workspace checks, Lean audits and real-Postgres tests pass;
the Goal Trigger lifecycle callback and intake callers remain downstream
integration work. The separate publication lifecycle slice adds exact-Fact
inverse selection and immutable publication-origin metadata.

The capability binds to one engine boot and the actual registered participant
descriptor. Minting rejects a descriptor with duplicate table names or any table
outside the boot's declared state surfaces. Commands must name that participant
and a nonempty duplicate-free subset of its tables. A unit has one fixed owner;
the permit binds the owner, participant and exact command tables, and the
Postgres adapter checks that stamped scope against the request before locking or
dispatching the payload. The command and participant payload must agree on the
owner. Lean's `Permit.engine` condition is realized by the private unit's
Engine reference and the capability-binding check before `BEGIN`; storage does
not carry a redundant engine tag that it could not independently verify.
Maintenance does not grant Fact, Abstraction, Perspective or Goal writes and
cannot change ordinary owner-role authorization. It remains in host code,
never `FlavorServices` or user-facing authoring handlers. Participant/table
scope does not distinguish command types within that participant.

Evidence: `BuiltProxima` already holds `SystemAuthority`
(`crates/proxima/src/runtime.rs:464`), but the existing owner-write gate still
requires resolved roles (`crates/core/src/engine/pipeline.rs:306`). The existing
host-state dispatch checks its registered participant and declared tables
(`crates/storage-pg/src/ports/write_session.rs:284`). The kernel adds a separate
authority domain; it does not weaken `Authorization.may_write`.

Rejected alternatives: a fixed deployment Group misses other owners; service
memberships cannot cover another user's personal owner; self-minted Admin
contexts manufacture ordinary authority; forwarding the existing system
witness alone does not establish cross-owner maintenance authority; a general
`OwnerWritePermit` would expose ordinary write capabilities to this path.

Green condition: Lean build and guarded theorem/axiom audits pass, followed by
runtime tests for foreign-engine, registration, table and owner rejection,
capability confinement, transaction rollback and owner-erasure serialization.
A maintenance permit usable for ordinary writes, or an accepted command outside
its owner/participant/table scope, falsifies the contract. Rollback is to stop
the host maintenance runtime and retain ordinary authorization unchanged;
there is no permission-widening fallback.

Accepted residuals: Lean does not prove Rust constructor privacy, boot-binding
freshness, faithful capture of actual storage registration, PostgreSQL atomicity
or shared owner lifecycle locking against exclusive erasure. It does not sandbox
trusted participant SQL: the participant must respect its declared tables and
stamped owner. These obligations require code review and runtime tests. The model
does not claim liveness or refine arbitrary SQL into its host-only transition.

## Transaction-bound lifecycle callbacks

The same boot-frozen participant may supply erase and export callbacks for its
declared lifecycle surfaces. The frozen request names whole-owner, source, or
exact-physical-Fact scope; exact physical IDs and original-publication copy
locators `(OwnerRef, MemoryId)` are separate typed selections. A source erase
can therefore remove a copy captured by an owner before transfer, while an
exact Fact erase removes copies of that physical `t` across original owners.
Only the existing authorized core hard-erase path can issue the latter
selection. The lifecycle source and publication-origin source are distinct
typed wrappers over the same native Proxima `SourceId` token; the erase adapter
passes the existing source value through by clone. It does not interpret the
CloudEvents producer `source` URI as a Fact source. This callback registration
grants no owner-erasure authority.

Ordinary host write units hold the shared database lifecycle fence from
transaction entry. Whole-owner, source, and custom physical erases acquire the
exclusive fence before owner/source/handle/target locks, then run the callback,
origin revocation, and core inverse in the same transaction. Production flavor
stores receive the opaque erase context captured from the actual frozen
registry; they must not reconstruct a narrower registry of their own.

Bulk erase sets a transaction-local five-second `lock_timeout` before asking
for the first exclusive fence. The wait conflict remains retryable at the
whole-transaction boundary; only after the fence is acquired does erase
disable the request-serving `statement_timeout` for its bulk work. This keeps
a queued first lock bounded without limiting a valid long erase body.

Callbacks are trusted transaction-local SQL, not a database sandbox. Receipts
must identify the exact declared tables and distinguish deleted rows from
scrubbed payloads. PostgreSQL commit and transport outcomes retain their normal
boundary: a server-rejected commit leaves the transaction unapplied, while
connection loss after commit dispatch can leave the caller's observed outcome
unknown even though core and host changes are atomic.
