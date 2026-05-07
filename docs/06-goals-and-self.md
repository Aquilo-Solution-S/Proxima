# 06. Goals And Self

Binding ADR:
`docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md`.

## Goal Entity

`Goal` is distinct from `Memory`.

```
Goal {
  goal_id: UUIDv7,
  owner: Owner,
  schema_id: SchemaId,
  schema_version: u32,
  title: text,
  text: text,
  state: Proposed | Active | Paused | Achieved | Abandoned | Rejected,
  supersedes: GoalId?,
  parent_goal_ids: GoalId[],
  authorship: User | System | External,
}
```

Lifecycle:

```
create -> Proposed | Active
supersede -> new Goal row + supersedes prior
state change -> new Goal row + supersedes prior
erase -> GDPR owner erasure only
```

No in-place mutation.

## Self

There is no global Self row.

Runtime self for one personality instance:

```
Self(instance) =
  personality_wake_config.current_self_perspective_memory_id
  + active owner Goals addressable to that self-Perspective
```

The self anchor is a Perspective row whose schema is declared by
`PersonalityFlavor::self_schema()`.

```
PersonalityInstance {
  personality_type_id: text,
  personality_instance_id: UUID,
  current_self_perspective_memory_id: MemoryId,
}
```

Self evolution:

```
self_Perspective_vN+1 --core/supersedes--> self_Perspective_vN
personality_wake_config.current_self_perspective_memory_id = vN+1
```

`personality_wake_config.status = tombstoned` removes the runtime
instance from dispatch and default listings. It does not delete the
self-Perspective, wake cursor, invocations, or authored A/P rows.
Tombstone is operational config (like wake-filter edits), not Memory
mutation; the instance's cognitive history stays queryable.

## Goal Assignment

Goal-to-personality assignment is an edge:

```
Goal memory or Goal entity
  --core/inspires-->
self-Perspective memory
```

`core/inspires` wakes the addressed personality through the auto-added
`OnEdge(core/inspires -> SelfPerspective)` filter.

Assignment does not imply obligation. The personality may read, ignore,
accept, counter, or write a different perspective.

## Separation

| Surface | Holds |
|---|---|
| Goal | desired direction, DAG, lifecycle |
| Wake config | operational policy, filters, probabilities |
| Self-Perspective | identity anchor for one instance |
| PersonalityFlavor | prompt, tools, write allow-lists |

Goals do not carry wake policy.

## Authorship

System-authored Goals carry operator/tool provenance. Personality-authored
memory rows carry split personality identity:

```
personality_type_id
personality_instance_id
wake_chain_depth
```

Operator-invocation reproducibility is row metadata, not a citation.
