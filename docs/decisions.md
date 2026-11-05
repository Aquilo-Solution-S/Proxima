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

### A distinct sharing policy has its own owner

Nesting composes membership into an organizational tree. A selected set of
memories with a different audience belongs to a separate group owner, with
direct person memberships when the people already sit in other group trees.
Reuse an owner for sets that share its access policy. This permits many
independent collaboration groups while preserving the one-parent hierarchy.
It also makes sharing an ownership choice: a transfer moves the whole series
rather than adding another ACL. See
[sharing a set of memories](concepts.md#sharing-a-set-of-memories) for the
consequences and the enforcing transfer contract.

### Capabilities and delegation only narrow authority

A deployment's served surface and a caller's exact operation keys do not
grant data access; owner roles remain a separate gate. A delegated grant binds
one owner, one command, a role ceiling and an expiry, and is checked again at
redemption, so a queued job never outgrows the authority it was issued with.
See [delegated work](host-integration.md#delegated-work).

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

## Pins are node-owned

Only `origin` and `reference` pins exist. Their kind follows the operation, and
the node that declares a relation owns it. If a statement needs its own
meaning, it is a node rather than a new edge kind.

See [Concepts](concepts.md) for the current pin model.
