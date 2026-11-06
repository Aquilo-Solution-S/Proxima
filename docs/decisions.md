# Decisions
> Status: current

This page records durable architectural choices. Release history belongs in
release notes. Implementation details belong in rustdoc, migrations, or the
live catalog.

## Principles and behaviour have separate owners

The Lean kernel states domainless principles. Rust code and tests are the
behaviour. A principle change starts in the kernel, then code and tests follow.
This keeps the kernel meaningful without treating it as an executable claim
about every implementation detail.

Source: [Lean principles](https://github.com/Aquilo-Solution-S/proxiima/blob/main/docs/lean/Causa/Principles.lean).

## One owner per fact

| Fact | Owner |
|---|---|
| Settings | [Configuration](configuration.md) and its completeness check |
| Types, defaults, limits | Rustdoc |
| Tool and resource schemas | Live catalog: `tools/list`, `proxiima://tools`, OpenAPI |
| Tables | SQL migrations |
| Core functions, triggers, policies | `crates/storage-pg/definitions/` |
| Invariants | Lean kernel |
| Release changes | Release notes and `Upgrade:` footers |

When a description is needed elsewhere, link to its owner. Do not copy the
table, schema, or default.

## Build-time spiice composition

A spiice is a vocabulary crate linked into a host at build time. A host reads
remote stores through `OtherStores`. This makes composition visible to the
compiler and keeps storage ownership at the host boundary. See [Build a
spiice](build-a-spiice.md) for the current integration surface.

## Identity and ownership have separate jobs

Authentication establishes the principal; ownership selects the data scope.
Each entity has one owner, resolved to the caller's role. Sharing uses group
membership or explicit transfer rather than entity-side ACLs. This keeps a
private note private even when its writer also administers a project group.
Personal ownership is derived from identity and its membership cannot be
edited. Organization structure and billing belong to the host.

The [identity and ownership map](concepts.md#identity-and-ownership) explains
the model. The principle is stated in
[Owner](https://github.com/Aquilo-Solution-S/proxiima/blob/main/docs/lean/Causa/Owner.lean)
and [Authorization](https://github.com/Aquilo-Solution-S/proxiima/blob/main/docs/lean/Causa/Authorization.lean);
runtime owner resolution and command ports enforce the implementation.

### Nesting composes access toward the parent

A subgroup contributes its members to its parent under a capability cap.
Caps weaken each path; independent memberships combine their capabilities.
This lets a team read department-wide knowledge without giving every department
member access to the team's work. The runtime rejects multiple parents and cycles;
the database maintains the effective-role projection. See
[nested groups](concepts.md#nested-groups-access-travels-upward) for the
direction, examples, and executable evidence.

### Durable ownership and Goal collaboration have separate scopes

Nesting composes membership into an organizational tree. A body of knowledge
that needs a different durable owner policy belongs to a separate group, with
direct person memberships when people already sit in other group trees.
Reuse an owner for sets with the same policy. A transfer moves the entire
series and its responsibility. The [Goal collaboration design](#goal-collaboration-keeps-one-durable-owner)
adds selected version publications and delegated work without changing the
owner or the one-parent hierarchy. Goal routes are additive: confidential
data that must be hidden from existing owner readers needs its own owner. See
[sharing a set of memories](concepts.md#sharing-a-set-of-memories) for the
consequences and the enforcing transfer contract.

### Capabilities and delegation only narrow authority

A deployment's served surface and a caller's exact operation keys do not
grant data access. Owner roles remain a separate gate. A delegated grant
additionally binds one owner, one command, a non-managing role ceiling, and
finite expiry. Fresh checks at redemption prevent a queued job from gaining
rights through a later promotion or running after a demotion below its
recorded ceiling. The opaque redeemed phase bounds in-process execution;
revoking the grant prevents future redemption rather than cancelling work
already admitted. See [delegated work](host-integration.md#delegated-work)
for its enforcing types and service.

### Attribution survives authority changes

Stored `by` and `via` identify the attested writer of each version. They do
not grant access and do not change when ownership transfers. This retains
history without making a former writer a permanent reader. A transport
session, a writer's caller/session UUID, and a delegation grant have distinct
lifecycles. See [requests, sessions, and workers](concepts.md#requests-sessions-and-workers).

## The catalog is the contract

Each capability is one tool action or resource. MCP and REST are two views of
one catalog. Clients discover the current catalog at runtime; documentation
does not become a second schema registry.

The live catalog is described by [Protocol](protocol.md).

## Goals express intent

A Goal owns its desired end-state, owner, state, and dependencies. Scheduling,
wake policy, and execution belong to the host. See [Concepts](concepts.md) and
[Host integration](host-integration.md) for the current boundaries.

### Motivation and guidance are versioned references

A Goal version names the Perspective versions that motivated it. A new
Perspective version names the Goal versions that guided it. The declaring
version owns each statement, and both roles are ordinary Reference pins.
This supports feedback across versions without retargeting history or making
motivation evidence. New causal targets share the source's owner; existing
F/A/P grounding and Fact/Abstraction evidence rules remain separate.

The version fields and valid-store predicates are stated in
[Goals](https://github.com/Aquilo-Solution-S/proxiima/blob/main/docs/lean/Causa/Goals.lean)
and [Provenance](https://github.com/Aquilo-Solution-S/proxiima/blob/main/docs/lean/Causa/Provenance.lean).
Runtime constructors, source-owned pin projections, and database admission
enforce the implementation. Former singular assignments stay on their
original versions as historical data; they are not converted into assertions
of actual influence. See [Goal relationships](concepts.md#goals).

## Pins are node-owned

Only `origin` and `reference` pins exist. Their kind follows the operation, and
the node that declares a relation owns it. If a statement needs its own
meaning, it is a node rather than a new edge kind.

See [Concepts](concepts.md) for the current pin model.

## Schema migrations are spiice-defined representation changes

A spiice declares the relation between old and new typed records, including
split and merge cases, and supplies the executable conversion. Core executes
that path before serving the target representation. The Lean theorem states
explicit source coverage, owner preservation, mapped pins, provenance, Goal
constraints, and retained erase-witness obligations, then derives retained
store validity; it does not choose conversion semantics or turn a
representation change into a business event.

A migration may replace admission identities when the spiice supplies the
complete mapping. Captured publication bytes stay unchanged. The internal
publication mapping relates each old capture to all replacement memories;
authorized erase of a replacement removes the associated capture, including
captures retained at a previous owner. This relation grants no read or write
authority and is not an ordinary-read alias.

The contract is [SchemaEvolution](lean/Causa/SchemaEvolution.lean). Replay
keys, catalog registration, migration execution, and recovery remain runtime
contracts and executable tests because they are not kernel fields.

Prior art: Confluent's [data contracts](https://docs.confluent.io/platform/current/schema-registry/fundamentals/data-contracts.html)
attach transformation rules and change metadata to schema versions. Here the
spiice supplies that meaning through typed migrations over persisted records.
The execution fence uses PostgreSQL's [transaction and advisory locks](https://www.postgresql.org/docs/current/explicit-locking.html):
source writes and retirement serialize at the database boundary, and completion
commits with the transformed records.

## Goal collaboration keeps one durable owner

Goal-level write includes the title and progress of that existing Goal.
Creating Goals and changing their decomposition still require owner authority;
a guest grant never becomes owner-wide Goal write.

The target collaboration model keeps each Goal and memory series under one
personal or Group owner. Context, exact-admission publication, and series work bindings
have distinct authority. Goal topology and semantic pins do not grant access.
N:M publication and explicitly delegated N:M editing support overlapping work
without changing ownership. Independent adaptations fork an exact source
version into a new series; later refinements revise that series.

Facts stay immutable observations. A publication identifies a Fact itself or
an exact Abstraction/Perspective version. Private revisions cannot replace a
previously published admission in the audience's view. Goal access is additive
and cannot restrict existing owner membership. Completion is separate from
revocation and erasure. Source-owner/controller changes must invalidate prior
authorisations without A-to-B-to-A resurrection.

The [collaboration model](concepts.md#goal-collaboration) owns the explanation
and examples. Its permission contract is stated in the kernel and enforced
by the runtime's typed command permits and owner-scoped storage paths.

Publication consent covers the Goal’s managed audience, including future
invitees. Existing Roles on the Goal's owning Group apply automatically,
alongside explicit Goal grants; their permissions combine. Source-owner and Goal-controller transfers invalidate existing
consent. A cross-owner fork requires explicit source-owner fork permission
and destination write permission.
