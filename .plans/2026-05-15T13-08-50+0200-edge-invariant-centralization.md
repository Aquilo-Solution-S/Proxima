# Centralized Edge Invariant Enforcement

Status: Done
Created: 2026-05-15
Reviewed: 2026-05-15
Implemented: 2026-05-15
Implementation:
- Centralized relation masks in core, DB edge invariant trigger, docs alignment,
  and spread local-check cleanup.
Verification:
- `cargo test -p proxima-core relation`
- `cargo test -p proxima-storage-pg --test edge_invariants_pg -- --test-threads=1`
- `cargo test -p proxima-storage-pg --test persist_wake_trace -- --test-threads=1`
- `cargo test -p proxima-flavor-goal --test accept_decline_pg -- --test-threads=1`
- `cargo test -p proxima-mcp-substrate --test tools_smoke -- --test-threads=1`
- `cargo test -p proxima-code --test workspace_run_pg -- --test-threads=1`
- `cargo check --workspace`
Notes:
- User decision: direct semantic/causal Fact-to-Fact links are forbidden;
  cross-domain Fact synthesis must materialize as an Abstraction.

## Summary

Centralize edge legality in storage and core relation descriptors. Lock the
ontology rule that cross-domain Fact connections happen through Abstractions,
not direct semantic Fact-to-Fact edges.

## Goals

- Make the database reject universal edge invariant violations.
- Make `RelationDescriptor` carry source, target, and authorship masks.
- Remove duplicated local layering checks from tools.
- Keep structural/event-source Fact-to-Fact edges legal.
- Reject causal or interpretive Fact-to-Fact edges.

## Key Changes

- Facts from different domains may be input to one typed cross-domain
  Abstraction: `A_cross -> F1, F2, ...`.
- A Perspective may author or frame a cross-domain Abstraction with
  `core/authored(P -> A)`.
- A Perspective is not provenance input to an Abstraction; provenance remains
  top-down from the produced Abstraction to its Fact or Abstraction inputs.
- Direct Fact-to-Fact remains valid only for mechanical relations such as
  structural source edges and provenance lineage.

## Implementation

- Add entity-kind and authorship masks to `RelationDescriptor`.
- Validate descriptor masks from the frozen registry before every edge write.
- Add a storage migration with an `edges` trigger that checks endpoint kind
  truth, owner equality, F/A/P layer order, no Fact supersession, and no
  causal/interpretive Fact-to-Fact edges.
- Update all registered relations with explicit masks.
- Reclassify `proxima-mcp/agent-link-refers-to` as `Interpretive`; its masks
  disallow Fact-to-Fact.
- Remove MCP-local layer assertions and rely on the central validator.
- Remove or reshape existing invalid edge writes, especially wake-trace
  Fact-to-Perspective provenance.

## Docs Alignment

- Update `docs/universe.md`, `docs/02-memory.md`,
  `docs/04-consolidation.md`, `docs/08-core-and-flavors.md`,
  `flavors/goal/SPEC.md`, and `AGENTS.md`.
- Replace the old `P x F x F -> Edge` channel with `P -> A_cross -> F*`.
- Update invariant 20 so F-to-A stays exclusive per output schema/operator
  while allowing multi-domain Fact input sets for explicit cross-domain
  Abstractions.

## Tests

- `cargo test -p proxima-core relation`
- `cargo test -p proxima-storage-pg --test edge_invariants_pg -- --test-threads=1`
- `cargo test -p proxima-storage-pg --test persist_wake_trace -- --test-threads=1`
- `cargo test -p proxima-flavor-goal --test accept_decline_pg -- --test-threads=1`
- `cargo test -p proxima-mcp-substrate --test tools_smoke -- --test-threads=1`
- `cargo test -p proxima-code --test workspace_run_pg -- --test-threads=1`
- `cargo check --workspace`

## Assumptions

- Domain remains schema/flavor semantic scope; no new `domain_id` column.
- Relation masks are build-time registry state, not a runtime relation table.
- gRPC/Tauri/TypeScript relation listing remains unchanged in this pass.
