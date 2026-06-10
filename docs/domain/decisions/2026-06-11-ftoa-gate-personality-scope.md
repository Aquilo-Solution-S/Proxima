# Decision: the F→A gate is personality-scoped

**Date:** 2026-06-11 (review round r2, codex finding)
**Status:** decided — flagged for Heinrich's review

## The tension

- doc 02 §Personality: "Multiple personality instances may be active for one
  Owner. Same Facts or Abstractions under different instances produce
  parallel lineages."
- doc 04 §Phase 2: F→A is "exclusive per (input contract, operator id,
  output Abstraction schema)" within a source batch.

Read together naively, the exclusivity would forbid two personalities from
producing parallel Abstractions over the same batch with the same operator and
output schema — contradicting the parallel-lineage rule.

## Resolution

Doc 04's own `source_batch_f2a` gate table resolves it: its columns include
"personality instance | runtime authoring context, **if the operator is
personality-bound**". The gate is therefore scoped per personality instance
where one exists. The kernel's `ftoa_batch_exclusive` now carries
`memory_authoring_personality m1 = memory_authoring_personality m2` as a
premise: personality-bound operators get parallel lineages (different
instances → premise fails → no identification); non-personality-bound
operators carry `none = none` and keep the strict exclusivity doc 04's
§Phase 2 sentence describes.

## Alternative for renegotiation

If F→A is intended to be personality-FREE always (one Abstraction per batch ×
contract × operator × schema regardless of personality), remove the premise —
but then doc 02's parallel-lineage sentence must be narrowed in writing to
A→P/A→Goal only.
