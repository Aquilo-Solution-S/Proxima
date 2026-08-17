# 02 — Memory

Memory is the cognitive graph above source-ingested Facts.

```
Reality ──FactIngest──► Fact ──F→A──► Abstraction ──A→P──► Perspective
                                            └────A→Goal────► Goal
```

## Ontology

Four node kinds. Citation is provenance, not a node.

| Term | What it is | Schema | Produced by |
|---|---|---|---|
| **Fact** | An admitted observation. Never revised. | `memory` (`kind = fact`) + optional sidecar | FactIngest |
| **Abstraction** | A re-derivable interpretation over Facts. | `memory` (`kind = abstraction`) + required sidecar | F→A |
| **Perspective** | A re-derivable integration over Abstractions. Self is a query, not a row. | `memory` (`kind = perspective`) + required sidecar | A→P |
| **Goal** | A desired end-state with a lifecycle. | `goal` | GoalWrite / A→Goal |
| **Citation** | Bibliographic proof. Not a node. | `memory.blob_id` 0..1 → `blob` | attached at write |

Identity is timeseries: `(handle, t)`. `handle` is the series. `t` is this version (uuidv7, `UNIQUE`, the row id). Schema lives on `memory_head` / `goal_head` only. Head `t` is display/search, not identity.

## The Layering Principle

```
ℓ(Fact)=0   ℓ(Abstraction)=1   ℓ(Perspective)=2
```

| Operator | Signature |
|---|---|
| F→A | `2^F × Π → A` |
| A→P | `2^A × Π → P` |
| frame | `P × A_cross → P` (payload references; no standalone pin kind) |
| A→Goal | `2^A × Π → Goal` |

`Π` = active Perspective context.

Forbidden: A→F, P→A, P→F writes. Upward pins (Fact→Abstraction, Fact→Perspective, Abstraction→Perspective). Facts as interpretation sources. Mutation of existing rows.

Facts are accepted, not revised. A/P are re-derivable. Perspective changes affect future writes, not existing Facts.

## The Core Entity

| Field | Rule |
|---|---|
| `handle` | series id |
| `t` | row id (`MemoryId`) |
| `kind` | `fact` / `abstraction` / `perspective`; must equal `memory_head.kind` |
| `owner_id` | NOT NULL FK → `owners` |
| `origins[]` | made-from pins (`t`). Empty on Facts. |
| `refs[]` | points-at pins from `references()` |
| `blob_id` | 0..1 citation. F/A only. |
| `source_id` / `ingest_key` | Facts only, both or neither. Replay key is `ingest_keys`. |

| Kind | Sidecar | Citation | Text |
|---|---|---|---|
| Fact | optional | `blob_id` | render on demand |
| Abstraction | required | `blob_id` | operator-authored |
| Perspective | required | none | operator-authored |

Fact identity is `t`, not content hash or ingest key. Same `(owner, source, ingest_key)` replays the same `(handle, t)`.

A/P provenance is `origins` (from `derived_from`). Authorship is a row column when present, not a pin kind.

## Edges

Pins connect Memories (and Goal columns pin `t`s). There is no edge table. [16](16-edges.md) is the reference.

> A pin carries no information beyond its existence: endpoints, direction, kind. Content lives in nodes.

```
memory.origins[]  -- Origin: what this row was made from
memory.refs[]     -- Reference: payload fields that point at other t
```

Two kinds. The kind follows the operation. No verb writes a pin.

| Kind | Lives in | Written by |
|---|---|---|
| `origin` | the derived write (`derived_from`) | that write's transaction |
| `reference` | schema-declared `references()` | ingest / derivation |

Rebuildability: re-deriving pins from node content yields the same set.

Admission: writer has write on the source and read on the target at write time. Address form is always a pin (`ReferenceBinding::Pin`). No follow-at-read.

F/A/P matrix (`origins` and `refs`):

| From → To | Legal |
|---|---:|
| Fact → Fact | yes |
| Abstraction → Fact | yes |
| Abstraction → Abstraction | yes |
| Perspective → Fact | yes |
| Perspective → Abstraction | yes |
| Perspective → Perspective | yes |
| Fact → Abstraction | no |
| Fact → Perspective | no |
| Abstraction → Perspective | no |

A causal claim is an interpretation Perspective, never a Fact source.

Source-owned: the declaring row owns the pin. Target may be another Owner. Unreadable targets redact independently; the pin stays if the source is readable.

## Causal Chain Query

```
chain(f, P_active)
  = refs among Facts
  + interpretation Perspectives under P_active
  + origins from contributing P/A to Facts
```

A query, not an entity. Different Perspectives produce different valid chains.

## Wake / Dream / Write

Dreaming is flavor-declared consolidation through ordinary wake/write. No Dream entity.

```
announce
  -> armed Active Goal wake match
  -> actor/tool-scope admission
  -> typed Memory / Goal writes (pins follow from what they declare)
```

Wake config is `wake_config` (N Goals share `wake_id`). Fire writes a `core/write-act-v1` Fact; produced Memory `refs` += that `t`.

## Re-derivation and Supersession

Facts never supersede. A/P/Goals may: a later `t` on the same `handle`. Old row stays. Head `t` moves.

Hard delete is abandonment-only (13). World is never abandoned. Forget cools to `cold/` and leaves `ingest_keys`.

Stateful Fact current-state is head-by-natural-key on the sidecar (03), not supersession.

## Assertion Lifecycle

Assertion = typed Abstraction. Evidence is `origins`. Subjects are `references()`. Current state is the head `t` of that handle.

## Perspective context and wake

Perspective is a typed memory row, not an authz carrier. Server-resolved Owner roles authorize. ChangeHistory pages `announce.seq`.

## Settled

- Strict F/A/P layering.
- Facts immutable; A/P/Goals append later `t` on the same handle.
- Cross-domain synthesis is a typed Abstraction.
- Citations are Fact ∪ Abstraction via `blob_id` (11).
- Two pin kinds; no verb writes a pin; rebuildable from node content.
- Causal chains and Self are queries (06).
- Dreaming is wake/write, not a substrate component.
