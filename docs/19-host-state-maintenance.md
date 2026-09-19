# 19 — Host-state maintenance authority

> **Status:** current.

Decision accepted by Heinrich on 2026-09-19 for Aquilo #8719/#8722: a host
capability may maintain its registered Goal Trigger participant's state for
all personal and Group owners. The kernel contract is implemented in
[`HostStateMaintenance.lean`](lean/Causa/HostStateMaintenance.lean), and the
Rust core, Postgres adapter and embedded-host boundary are implemented in the
current slice. Workspace checks, Lean audits and real-Postgres tests pass;
Aquilo's lifecycle caller integration remains pending.

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
