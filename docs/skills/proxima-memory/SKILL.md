---
name: proxima-memory
description: >-
  Use when an agent should recall or store a project's durable, cross-session
  knowledge through Proxima — a persistent shared-brain memory MCP (tools like
  core_remember / core_derive / core_interpret / core_search_memories / core_goal).
  Treat the intent as the trigger even when the user names no tool and never says
  "memory": recall what was already decided and which gotchas were hit BEFORE a
  refactor, architecture change, domain shift, or debugging in an unfamiliar area;
  save a decision, gotcha, or lesson so nobody relearns it; capture a recurring
  pattern across facts; relate or connect existing memories; update your stance or
  self-model about a project; set or decompose a goal; or consolidate what is
  worth keeping when wrapping up a session. Also trigger when unsure how to store
  something (remember vs derive vs interpret; fact vs abstraction vs perspective)
  or when looking for a verb that connects two memories and finding none. Do NOT
  trigger for one-off "don't-forget" reminders, casual or personal recall,
  CLAUDE.md or other local notes, or merely inspecting the memory system's own
  source code, schema, or database.
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
edge-kind table, the read-resource decision guide). Read `proxima://how-to` when you
want depth; this skill is what makes you actually turn the wheel.

> Tools below use the canonical names; your deployment may expose a subset (a
> profile can hide goals, code, or other tools). The server's `instructions` and
> `proxima://how-to` always reflect what is *actually* available — defer to them
> over this list, and never call a tool that is not advertised.

## The one hard law: no tool writes a connection

An edge carries no information beyond its existence, so every edge is a
consequence of what some node says: an `origin` entry from the handles a write
declares it was made from, a `reference` entry from a schema-declared payload
field. Nothing you call takes an edge kind as an argument, and there is no
connect verb to reach for.

**To relate Facts, derive over them:**

```
core_derive(kind="Abstraction", title=..., body=...,
            source_handles=["F:aaaa", "F:bbbb", "F:cccc"])
```

`source_handles` lands `origin` entries from the new Abstraction/Perspective
down to each source. **That is the graph.** Wanting to connect two `F:` handles
is the signal to *abstract*.

**A claim about memories that already exist is a Perspective, not an edge:**

```
core_interpret(claim="the outage followed the deploy", confidence=80,
               subjects=["F:aaaa", "A:bbbb"])
```

It returns a `P:` handle. A reason and a confidence are a judgment, and a
judgment is a Perspective; its subjects become that Perspective's own
references. A Fact never interprets — layering refuses a Fact as an
interpretation source.

## What to capture → which tool

| You want to… | Use |
|---|---|
| Record an observation / something that happened / a fact you learned | `core_remember` → Fact |
| Capture a recurring pattern, generalization, or lesson across ≥2 Facts | `core_derive` kind=**Abstraction**, `source_handles`=those Facts |
| Record or update a stance or self-model ("how I see X", "who I am") | `core_derive` kind=**Perspective** |
| **Relate / connect memories** | derive an Abstraction/Perspective over them — there is **no** connect verb |
| Claim what existing memories mean, with a confidence | `core_interpret` → interpretation Perspective |
| Set an intent / objective to pursue | `core_goal` actions `set` / `decompose` |
| Find prior knowledge | `core_search_memories` (hybrid default) -> read `proxima://memory/{id}?expand_neighbors=true` |
| Precision recall / big result sets | `core_search_memories` with `min_score` (drop weak hits), `semantic_weight` (retune hybrid fusion), and `cursor`=last `next_cursor` (page past the 50 cap) |

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
  abstracting. A deployment may bind a model identity to your token, in which
  case that identity is recorded and sending a *different* `model_id` is
  refused; omit it or send the bound value.
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

- **Hunting for a link/connect tool** → there is none, by design. To connect
  Facts, derive an Abstraction over them; to judge what they mean, interpret.
- **Storing a lesson or pattern as a Fact** → flattens Facts → Abstraction.
  Derive it, cite the source Facts.
- **Writing without recalling first** → duplicates and ungrounded work. Search
  first.
- **Waiting for an "engine" to abstract** → there is none. Deriving is your job.
- **Calling a tool your deployment does not advertise** → check the server
  `instructions` / `proxima://how-to` for the live tool surface.
