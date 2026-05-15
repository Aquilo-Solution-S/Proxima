# 04. Consolidation

Binding ADR:
`docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md`.

## Shape

```
Reality event
  -> change_event(seq, owner, entity | edge, authoring personality?, depth)
  -> wake entries per (Owner, personality_instance_id)
  -> decider(system_prompt, self-Perspective, tools, reads)
  -> memory writes + edge writes
  -> change_event(...)
```

Consolidation is the per-personality wake/decide/write loop.

## Runtime Tables

| Table | Key | Function |
|---|---|---|
| `personality` | `(Owner, instance_id)` | root Perspective pointer, `active | tombstoned` |
| `personality_wake_entries` | `(Owner, instance_id, wake_entry_id)` | trigger, recipe, tier, palettes, `active | needs_repair` |
| `personality_wake_cursor` | `(Owner, type_id, instance_id)` | last considered `change_event.seq` |
| `personality_wake_invocations` | `(Owner, type_id, instance_id, seq)` | idempotency for fired wakes |
| `memories` | `memory_id` | F/A/P row, split personality identity, `wake_chain_depth` |

`source_batches` remains EventSource lifecycle. Domain metadata belongs on
`CitedObject`, not the batch.

## Dispatcher Contract

For each active instance:

1. Read cursor.
2. Walk owner `change_event` rows with `seq > cursor`.
3. Reject self-authored events.
4. Reject events with `wake_chain_depth >= max_wake_chain_depth`.
5. Evaluate `WakeEntry` rows.
6. Apply deterministic probability hash `(seq, type_id, instance_id, wake_entry_id)`.
7. Insert invocation row before running the decider.
8. Run decider with substrate tool palette plus flavor tools.
9. Validate every write against declared schema/relation allow-lists and
   central edge invariants.
10. Append memory/edge rows atomically.
11. Advance cursor regardless of match or write output.

Cursor advancement is separate from invocation idempotency. Low-probability
instances do not re-walk old events.

## Writes

Personality writes:

| Write | Required |
|---|---|
| Abstraction | typed sidecar, text, model id, prompt version, split personality identity |
| Perspective | typed sidecar, text, model id, prompt version, split personality identity |
| Goal | Goal row, Goal sidecar, authorship |
| Edge | registered relation only |

Substrate auto-wires `core/derived-from` provenance from the triggering event
and tracked reads. Personalities cannot author `core/derived-from` or
`core/supersedes` directly.

Edge writes pass through the frozen relation descriptor and the storage trigger.
Tool-local checks may narrow inputs for UX, but must not duplicate the
load-bearing layer, owner, endpoint-kind, or semantic Fact-to-Fact rules.

## Chain Depth

```
external Fact depth = 0
personality write depth = max(trigger depth, read_log depths) + 1
wake allowed iff trigger depth < PersonalityFlavor::max_wake_chain_depth()
```

Cross-personality cycles terminate by depth bound.

## Wake Entries

`WakeEntry` match fields:

| Field | Match |
|---|---|
| `trigger_kind = on_memory` | `EntityAppend` memory schema |
| `trigger_kind = on_edge` | `EdgeAppend` relation |
| `authored_by` | `any | self_author | other` |

Stored entries carry `recipe_ref`, model tier, execution mode, palettes,
and `max_rounds`. Strict validation failure sets the entry
`disabled_reason`.

## Invariants

- Facts remain immutable observations.
- A/P outputs carry typed sidecars.
- Edges obey `layer(src) >= layer(tgt)`.
- Direct semantic/causal Fact-to-Fact edges are forbidden; use a typed
  Abstraction over the Fact set.
- Relation ids resolve through the frozen registry.
- Similarity never creates edges.
- Wake entries are operational policy; Goals are direction, not policy.
