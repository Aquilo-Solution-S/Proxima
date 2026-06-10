# Decision: batch-id uniqueness is scoped, not global

**Date:** 2026-06-11 (axiom-minimization pass)
**Status:** decided — flagged for Heinrich's review

## The finding

The transcription's `batch_unique_within_source_owner` axiom asserted GLOBAL
injectivity: any two events sharing a batch id share source AND owner. Every
doc statement scopes the property instead — doc 01 §The contract (Q6), doc 07
§ID Types, doc 04 all say "unique within `(source_id, owner)`". A per-scope
engine validation cannot establish the cross-scope implication; two owners
whose sources coincidentally declare the same batch UUID are doc-admitted.
(UUIDv7 makes real collisions practically impossible, but "practically
impossible" is not an ontology axiom.)

## Decision

1. The global axiom is REMOVED. The scoped validation is an engine check with
   no kernel-observable face (the kernel has no Batch entity to anchor
   per-scope uniqueness to).
2. The F→A gate (`ftoa_batch_exclusive`, Foundations.Operators) gains an
   explicit `memory_owner m1 = memory_owner m2` premise — it previously leaned
   on the global axiom to avoid identifying Abstractions across Owners. The
   gate's doc-04 scope columns (`owner`, `source batch`) support the owner
   dimension directly.

## Alternative for renegotiation

If Heinrich prefers global batch-id uniqueness as a deliberate strengthening
(engine generates UUIDs, collisions treated as corruption), reinstate the
axiom and drop the owner premise — one commit either way.
