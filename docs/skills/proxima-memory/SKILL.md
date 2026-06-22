---
name: proxima-memory
description: >-
  Use when reading from or writing to a Proxima shared-brain memory MCP (tools
  like core_remember / core_derive / core_link / core_search_memories /
  core_goal_set) — BEFORE architectural or domain work (recall first), when
  recording a learning or a recurring pattern, when relating memories to each
  other, when updating your stance or self-model, when setting a goal, or at any
  session break (consolidate). Also use whenever you are unsure whether to
  remember vs derive vs link, or you hit a "rejects source kind Fact" error.
  Trigger this even when the user does not say "memory": any time you are about to
  act in an unfamiliar area backed by a Proxima brain, or you have just learned
  something worth keeping.
---

# Proxima Memory

## Overview

Proxima is the **substrate** — storage, embeddings, the
Fact → Abstraction → Perspective structure, goals, retrieval. **You are the
cognition.** There is no autonomous engine deriving things for you: when you
remember, abstract, or reflect, *you* are the F→A / A→P operator. The brain only
helps if you query it before acting and feed it as you learn.

This skill is the **session ritual** — *when* to recall and consolidate. The
server documents its own *contract*: it returns an `instructions` block at
connect and exposes a `proxima://how-to` resource (worked examples, the
edge-class table, the read-tool decision guide). Read `proxima://how-to` when you
want depth; this skill is what makes you actually turn the wheel.

> Tools below use the canonical names; your deployment may expose a subset (a
> profile can hide goals, code, or other tools). The server's `instructions` and
> `proxima://how-to` always reflect what is *actually* available — defer to them
> over this list, and never call a tool that is not advertised.

## The one hard law: Facts cannot link Facts

`core_link` authors edges **only from an Abstraction or Perspective**. A Fact
source is rejected at storage: `relation core/agent-link-refers-to rejects source
kind Fact`. Facts are immutable observations — they do not interpret each other.

**To relate Facts, do not link them — derive over them:**

```
core_derive(kind="Abstraction", title=..., body=...,
            source_handles=["F:aaaa", "F:bbbb", "F:cccc"],
            model_id="<your-operator-label>")
```

`source_handles` auto-creates `derived-from` provenance edges from the new
Abstraction/Perspective down to each source. **That is the graph.** Wanting to
connect two `F:` handles is the signal to *abstract*, not to link. (`core_link`
is for the rarer case of one Abstraction/Perspective pointing at other memories.)

## What to capture → which tool

| You want to… | Use |
|---|---|
| Record an observation / something that happened / a fact you learned | `core_remember` → Fact |
| Capture a recurring pattern, generalization, or lesson across ≥2 Facts | `core_derive` kind=**Abstraction**, `source_handles`=those Facts |
| Record or update a stance or self-model ("how I see X", "who I am") | `core_derive` kind=**Perspective** |
| **Relate / connect memories** | derive an Abstraction/Perspective over them — **NOT** `core_link` between Facts |
| Set an intent / objective to pursue | `core_goal_set` (+ `core_goal_decompose`) |
| Find prior knowledge | `core_search_memories` (hybrid default) → `core_get_memory` (`expand_neighbors: true`) |

A generalization stored as a Fact flattens the hierarchy and loses its
grounding — derive it instead.

## Recall before you act

Before architectural decisions, domain shifts, or debugging in an unfamiliar
area: **search the brain first** (`core_search_memories`, hybrid). Pull your
Perspective + the relevant Facts. Memory you never query is dead weight — a
session that starts without a recall starts from zero. This is the single
highest-leverage habit: most duplicate or ungrounded work traces back to writing
before reading.

## Write discipline

- **`idempotency_key`** on every `remember` / `derive` you might replay (imports,
  re-runs) — keyed by a stable slug. Replays become no-ops, not duplicates.
- **Tag consistently** — domain + kind (e.g. `["<project>", "<subsystem>", "<kind>"]`)
  so hybrid and tag search cluster related memories.
- **`model_id` on derive** = your operator label — provenance for who did the
  abstracting.
- Store the durable **why**, not a transcript or what version control already
  records.

## Consolidate at natural breaks

After a chunk of work, turn the wheel (Facts → Abstraction → Perspective →
Goals):

1. `core_remember` the key Facts — decisions made, gotchas hit.
2. If a pattern recurred across those Facts, `core_derive` an Abstraction over
   them (cite the source Facts via `source_handles`).
3. When your stance genuinely shifts, update your Perspective.
4. If new intent emerged, set or refine a Goal.

Semantic search needs embeddings; the server drains them in-process when an
embedding client is configured, and degrades to lexical search otherwise — so a
just-written memory may take a moment to become semantically findable.

## Common mistakes

- **`core_link` from a Fact** → rejected. To connect Facts, derive an
  Abstraction over them.
- **Storing a lesson or pattern as a Fact** → flattens Facts → Abstraction.
  Derive it, cite the source Facts.
- **Writing without recalling first** → duplicates and ungrounded work. Search
  first.
- **Waiting for an "engine" to abstract** → there is none. Deriving is your job.
- **Calling a tool your deployment does not advertise** → check the server
  `instructions` / `proxima://how-to` for the live tool surface.
