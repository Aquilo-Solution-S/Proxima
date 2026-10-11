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
scope does not distinguish command types within that participant: a host
registers exactly one participant (a second registration refuses boot), and a
participant serving several command types dispatches with
`HostStateRequest::is::<C>()` / `try_downcast::<C>()`, which returns the
request unchanged on a mismatch, or is a `PgCommandDispatcher` (below).

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

## Typed command dispatch

`PgCommandDispatcher` (Host API, `proxima::host`) is the host's one
`PgHostStateParticipant` for several command types. It adds no per-command
authority: scope stays the participant and its declared tables.

```rust
PgCommandDispatcher::new(participant_id, declared_tables)
    .register::<C, _>(handler)?          // C: HostStatePayloadOwners
    .with_lifecycle_port(port)           // optional, as for any participant
```

| Piece | Contract |
|---|---|
| `HostStatePayloadOwners: HostStateCommand` | `payload_owners() -> Vec<Owner>`: every owner the payload names, empty when none. A separate trait, so `HostStateCommand` and its implementors are unchanged; the method is required, so the command author states the answer |
| `HostStateHandler<C: HostStateCommand>` | `handle(tx, permit, command: AgreedCommand<C>) -> Result<HostStateOutcome<C::Outcome>, StorageError>`. The dispatcher builds the `HostStateReply` from the typed outcome, so the reply cannot disagree with `C::Outcome` (a `compile_fail` doctest holds it). The handler gets the typed command, never the `HostStateRequest` |
| `AgreedCommand<C>` | `Deref<Target = C>` and `into_inner()`. Built only by the dispatcher, after the owner comparison below succeeded: no public constructor, `Clone` or `Default` (a `compile_fail` doctest holds it). Every command a handler receives had its owners compared with a permit, and the dispatcher passes that permit. Not bound to a permit: host code that keeps one and calls `handle` itself with another permit is outside the guarantee |
| `register` | Boot-time `CommandRegistrationError`: `ForeignParticipant` (`C::PARTICIPANT_ID` is not the dispatcher's), `UndeclaredTable` (`C::TABLES` outside `declared_tables`), `DuplicateCommand` (type already registered). A command that implements no `HostStatePayloadOwners` does not compile (`compile_fail` doctest) |
| `apply` | Tries each handler with `try_downcast::<C>()` in registration order. A request no handler accepts is a `ConstraintViolation` naming the participant; no SQL runs |

`PayloadAllowed` (`HostStateMaintenance.lean`) is enforced once, in the
dispatcher, before the matching handler runs: `command.owner()` and every
`payload_owners()` entry must equal `permit.owner()`. A difference is a
`ConstraintViolation` ("host-state command owner does not match write permit"),
which poisons the unit as any participant error does. The engine's permit
machinery is unchanged: participant, tables and owner are stamped and checked
before the dispatcher is reached. The kernel does not sandbox participant SQL,
and this does not change that: a handler still stamps the permit's owner and
touches only its command's declared tables.

## Transaction-bound lifecycle callbacks

The same boot-frozen participant may supply erase and export callbacks for its
declared lifecycle surfaces. The frozen request names whole-owner, source, or
exact-physical-Fact scope; exact physical IDs and original-publication copy
locators `(OwnerRef, MemoryId)` are separate typed selections. A source erase
can therefore remove a copy captured by an owner before transfer, while an
exact Fact erase removes copies of that physical `t` across original owners.
Only an authorized core hard erase issues the latter selection: whole-owner,
source-scope, or the flavor-scoped erase ([13 §Flavor-scoped
erase](13-compliance.md#flavor-scoped-erase)). The lifecycle source and publication-origin source are distinct
typed wrappers over the same native Proxima `SourceId` token; the erase adapter
passes the existing source value through by clone. It does not interpret the
CloudEvents producer `source` URI as a Fact source. This callback registration
grants no owner-erasure authority.

Ordinary host write units hold the shared database lifecycle fence from
transaction entry. Whole-owner, source, and flavor-scoped erases acquire the
exclusive fence before owner/source/handle/target locks, then run the callback,
origin revocation, and core inverse in the same transaction. The flavor-scoped
erase runs on the Engine's own storage, whose erase context is captured from
the actual frozen registry; a flavor never holds one.

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
