# Surface redesign: dev-first store explorer + runtime dashboard

**Status:** design
**Date:** 2026-05-10
**Owner:** Heinrich
**Related:**
- `docs/09-frontend.md` (Solid + Tauri stack, schema-driven UI codegen, Hub)
- `docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md`
  (wake/decide/write loop; runtime objects this view inspects)
- `docs/superpowers/specs/2026-05-10-personality-topology-canvas-design.md`
  (Atlas-side lineage canvas — the "cool" view; Surface defers all
  graph-shaped questions there)
- `packages/frontend-core/src/views/surface.tsx` (current 863-line
  three-section view being replaced)
- `packages/frontend-core/src/hub.ts:63-197`
  (`registerRenderer` / `rendererFor` schema dispatch — kept verbatim)
- `packages/frontend-core/src/graph-filter-store.tsx`,
  `graph-selectors.ts` (existing filter plumbing — extended, not
  replaced)

## Problem

The current Surface tab stacks `Perspectives` / `Abstractions` / `Facts`
as three independent list-and-detail panes filling the center column,
with `GOAL DAG`, `EVENT STREAM`, and `Filters` as collapsible side
rails. Three pillar sections compete for vertical real estate; a
2360-fact list gets the same height as a 7-perspective list. Filters
are hidden behind a collapsed right rail. Goals are demoted to a
left-side DAG drawer, breaking the four-pillar peer ontology.
Schema-aware rendering exists (`Hub.rendererFor`) but is wired only
into a few sites and never paired with a uniform metadata + lineage
shell. The view tries to be both an ontological pedagogy and a dev
explorer; it succeeds at neither.

We picked a target audience: **the default Shell Surface is for devs
debugging the engine and its store.** Flavor-specific opinionated UIs
live in their own tabs (e.g. the existing `Code` tab), not on Surface.
Custom flavor frontends — eventually a separate distribution — are
free to think completely differently. Surface is uniform on purpose.

## What this spec is — and isn't

**Is:** a single-pane explorer with pillar tabs, a filter rail (chips
+ drawer), and a generic schema-driven inspector. A faceted DB-browser
shape grafted onto the existing Solid + Tauri stack.

**Is not:**
- Atlas. No graph rendering on Surface. The detail pane shows a 1-hop
  lineage block as text counts with drillable links; the actual
  lineage graph is Atlas's job.
- A flavor frontend. Surface does not load flavor-shipped React. Every
  detail pane uses the same schema-driven renderer wired through
  `Hub.rendererFor`. Flavors that want bespoke UIs ship their own tab.
- A query language. `⌘K` palette is deferred. v1 ships chips + drawer.
- A timeline. The `Atlas` view and the topology canvas already cover
  temporal/topological storytelling. Surface treats time as one facet,
  not the spine.

## Layout

```
┌─ Surface ──────────────────────────────────────────────┐
│ All 2485 │ P 7 │ A 118 │ F 2360 │ G 0      ⚙ Filters  │  ← tab strip
├────────────────────────────────────────────────────────┤
│ [flavor: proxima-code ✕] [schema: code-chunk-v1 ✕] +  │  ← chip rail
├────────────────────────────────────────────────────────┤
│ 019e117d  commit-summary-v1   A   495b  rust    2m   │
│ 019e117c  code-chunk-v1       F  1052b  rust    2m   │
│ ...                                              ┌────│  ← list  │ detail
│                                                  │    │
├────────────────────────────────────────────────────────┤
│ ◉ idle · last wake 2m · 3 wakes/hr · 2 active pers.  │  ← activity strip
└────────────────────────────────────────────────────────┘
```

Top-down regions:

1. **Tab strip** — `All | P | A | F | G` plus a right-aligned `⚙
   Filters` toggle. Counts live next to each tab label. Pillar is
   implicit on per-pillar tabs (no `pillar=` chip), explicit-as-badge
   on `All`.
2. **Chip rail** — current filter state. Each chip removable.
   `+ add` opens a small popover for one-off facet picks; `⚙ Filters`
   opens the full drawer.
3. **List + detail split** — center pane is a virtualized list (reuse
   `VirtualList`), right pane is the generic inspector. Drag-to-resize
   between them with persisted width.
4. **Activity strip** — last 3 engine events at a glance.
   Click expands the existing `EventStream` drawer at full height.

The four center constants from the current `surface.tsx`
(`SURFACE_CENTER_MIN_WIDTH`, `RAIL_COLLAPSED_WIDTH`, etc.) carry over;
the `GOAL_RAIL_*` constants retire.

## Filter rail

**Two surfaces, two jobs.**

- **Chips = state.** Every active filter renders as a removable chip
  above the list. Order is canonicalized (flavor → schema → author →
  batch → time → size). Chip text matches the drawer's serialized form
  so users can map a chip back to a drawer field at a glance.
- **Drawer = constructor.** Slide-in panel on the right (`⚙ Filters`
  or `⌘F`). Form-shaped, never auto-applies — explicit `Apply` button.
  Sticky-open per-session if the user wants it (keyboard `⌘\`
  toggles).

Drawer fields, v1:

| Facet         | Type                    | Source                                                                |
| ------------- | ----------------------- | --------------------------------------------------------------------- |
| `flavor`      | multi-select            | `Hub.flavorFor(schema_id, schema_version)` over loaded schemas        |
| `schema_id`   | multi-select (typeahead)| distinct `(schema_id, schema_version)` in `memoriesById` + `goalsById`|
| `authored_by` | multi-select            | join `memoriesById` ↔ `eventsBySeq.authoring_personality_instance_id` |
| `time`        | range picker            | ULID-decoded ms from creating-event `seq`                             |
| `size`        | numeric range           | `MemoryRow.payload.length` client-side                                |
| `pillar`      | checkbox set, P/A/F/G   | only visible on `All` tab                                             |

`batch` (`source_batch_id`) is intentionally absent from v1 — it is not
projected onto `ChangeEvent.EntityAppend` today. See "Out of v1."

The drawer composes a single `GraphFilter` value (extension of the
existing `useGraphFilter` store). `filterGraphSnapshot` already takes
a filter; v1 work is widening the filter shape, adding the
`memory_id → creating_seq` provenance map to the graph store, and
adding a ULID-timestamp helper. No new transport surface.

Chip removal mutates the same store; the drawer reads from it on open
so it always reflects current state.

## Detail pane (generic, schema-driven)

Three fixed blocks, in this order, for every selected row regardless
of pillar or flavor:

```
proxima-code/code-chunk-v1   v1
019e117c · 1052 bytes · personality-rust

PAYLOAD
  state      Present
  chunk      1
  type       block
  language   rust
  path       crates/core/tests/...rs
  range      53-80

LINEAGE  (1-hop)
  → informs A commit-summary-v1 ×1
  ← from   F file-revision-v1 ×1

METADATA
  schema_id    proxima-code/code-chunk-v1
  flavor       proxima-code
  batch        019e117c
  written_at   2m ago
```

**Header.** Schema id + version, ULID short-form, byte size,
`authored_by` label.

**PAYLOAD.** The decoded payload, rendered via `Hub.rendererFor(schema_id,
schema_version)`. The Hub already exposes per-schema renderers
(currently used in three sites — `surface.tsx:38`, `surface-events.tsx:26`,
`atlas/inspector.tsx:463`); v1 wires every Surface detail through this
same path. These renderers are **build-time** Solid components compiled
into the Shell bundle — not flavor-shipped runtime UI. The "generic
everywhere" rule applies to the *outer shell* (header + PAYLOAD /
LINEAGE / METADATA frame, uniform across schemas); inside PAYLOAD, the
existing build-time schema dispatch picks per-schema field formatting
(timestamp prettifying, code-block highlighting, hash truncation, etc.).
When no renderer is registered, the fallback is a flat key/value list
of the decoded CBOR object — same field grammar as the registered
renderers produce, without per-field formatting hints.

**LINEAGE.** A 1-hop neighborhood as text counts with drillable links.
For each direction (`outbound`, `inbound`), group by edge `relation_id`
and show `relation arrow_label · target_pillar target_schema_id ×N`.
Clicking a target schema id swaps the chip rail to that schema and
clears row selection — drill-down without navigating away from
Surface. "Open in Atlas" link in the block header for graph view.
Lineage is a pure frontend selector `oneHopLineage(memoryId, edgesById,
memoriesById)` in `graph-selectors.ts`; the graph store already loads
`edgesById` so no new transport surface is needed. Counts are grouped
by `(direction, relation, target_pillar, target_schema_id)`.

**METADATA.** Fully decoded envelope: `schema_id`, `schema_version`,
`flavor`, `pillar`, `authored_by` (from joined ChangeEvent),
`written_at` (ULID-decoded from creating event `seq`), `byte_size`
(payload length). This block is identical across every schema; it's
the row's "stat sheet." `source_batch_id` is omitted in v1 (see "Out
of v1").

No flavor-shipped React anywhere on this surface. Renderers come from
the same Hub the rest of the app uses; flavors register on engine
boot per `09-frontend.md`. Custom flavor UIs live in their own tabs.

## Tabs and rows

**`All` tab columns** (default sort: `written_at desc`):

| col       | width  | source                                              |
| --------- | ------ | --------------------------------------------------- |
| pillar    | 24px   | P/A/F/G colored badge                               |
| schema    | flex   | `schema_id` (flavor prefix muted)                   |
| author    | 160px  | `authored_by` short label                           |
| size      | 64px   | bytes, right-aligned                                |
| time      | 80px   | relative, from ULID-decoded creating-event `seq`    |

**Per-pillar tabs** (`P`, `A`, `F`, `G`) drop the pillar column and
gain ~24px back. Same other columns. Sort header is clickable on
schema, size, time. Multi-column sort is out of v1 scope.

`All` tab is the default on cold open. Last selected tab persists per
window via the existing settings store.

## Activity strip and Event Stream

Bottom strip is always present, single-line, three slots:

```
◉ <state> · last wake <relative> · <rate> · <N> active personalities
```

`<state>` ∈ `idle | waking | deciding | writing | error`. Slots are
elided when zero. Click anywhere on the strip → expand the
**Event Stream** drawer (currently `EventStream` in
`surface-events.tsx`) full-height on the right; left arrow on the
drawer's header collapses it back. Activity strip itself is
non-scrollable; the drawer has the scrollable, filterable feed.

Event Stream is the only place runtime events are paginable; it
retains its current schema-aware rendering via `Hub.rendererFor`.

The `Goal DAG` left rail retires from Surface entirely. The `G` tab
covers Goal browse; the canvas-of-goals view (if needed) belongs in
Atlas, not Surface.

## Keyboard

| Key            | Action                                  |
| -------------- | --------------------------------------- |
| `⌘1`–`⌘5`      | Switch tab (`All` / `P` / `A` / `F` / `G`) |
| `⌘F`           | Toggle filter drawer                    |
| `⌘\`           | Toggle drawer sticky-open               |
| `⌘E`           | Toggle Event Stream drawer              |
| `↑` / `↓`      | Move row selection                      |
| `Enter`        | Open detail (already auto-shown on focus)|
| `Esc`          | Close drawer or clear chip focus        |
| `⌘Backspace`   | Remove last chip                        |

`⌘K` palette is reserved but not implemented in v1.

## Mental-model recap

What the redesigned Surface tells the user without prose:

- **Four pillars are peers.** Tab strip shows `P`, `A`, `F`, `G`
  side-by-side with counts. No pillar lives in a side rail.
- **The store has many axes.** Chips above the list say "you're
  looking at a slice"; the drawer says "here are all the axes."
  Pillar is one axis among many, foregrounded only on the `All` tab.
- **Every row has the same envelope.** Detail pane's PAYLOAD /
  LINEAGE / METADATA shape is identical regardless of flavor. Devs
  build one mental model, not one per schema.
- **The engine is alive.** Activity strip ticks even when the user is
  not interacting; the Event Stream drawer is one click away. Surface
  is not just a store browser — it answers "what is the engine doing
  *right now*."
- **Lineage exists, but graph view lives elsewhere.** 1-hop counts on
  the detail pane are enough to seed the question "who informs whom";
  the answer in graph form is Atlas.

## v1 scope

- Tab strip (`All | P | A | F | G`) + counts
- Chip rail above virtualized list
- Filter drawer with six facets (flavor, schema_id, authored_by,
  time, size, pillar)
- Provenance index (`memory_id → creating ChangeEvent.seq`) on
  graph-store ingestion + ULID-timestamp helper
- Generic three-block detail pane (PAYLOAD / LINEAGE / METADATA)
- 1-hop lineage selector in `graph-selectors.ts` (pure frontend; reuses
  already-loaded `edgesById`)
- Activity strip + existing Event Stream drawer expansion
- Keyboard map above
- Retire Goal DAG rail; ensure `G` tab covers Goal browse parity

Out of v1:
- `batch` / `source_batch_id` facet — needs engine-side projection of
  `source_batch_id` onto `ChangeEvent.EntityAppend` (and matching
  `EdgeAppend`) before the frontend can filter / display it. Tracked
  for v1.1.
- `⌘K` palette / query language
- Multi-column sort
- Flavor-shipped React renderers (always out — design choice)
- Saved filter presets
- Cross-batch diffing
- Goal-payload aware row columns (defer until typed-Goal-payload work
  in `project_typed_goals` lands)

## Migration from current Surface

`packages/frontend-core/src/views/surface.tsx` (863 lines) is the
single artifact replaced. Plan-time decomposition the writing-plans
skill should produce:

1. Add the provenance index to `graph-store.tsx`: a parallel
   `memoryProvenance: Map<memory_id, { creating_seq, authoring_personality_instance_id, written_at_ms }>`
   populated on `EntityAppend` ingest and on bootstrap event seed.
   New `ulid.ts` module exposing `ulidTimestampMs(ulid)` for the
   `seq` decode.
2. Widen `GraphFilterState` to include `authoredBy: ReadonlySet<string>`,
   `timeRange: { fromMs, toMs } | null`, `sizeRange: { minBytes, maxBytes } | null`.
   Update `filterGraphSnapshot` to honor all three using the new
   provenance index.
3. Carve the existing `<Section>` repetition out of `surface.tsx` into
   a single `<RowList>` driven by the unified filter store. New file
   `views/surface/row-list.tsx`.
4. Lift goal/event rail width logic out of `surface.tsx`; the goal
   rail dies entirely, the event rail becomes the activity strip
   expansion. New file `views/surface/activity-strip.tsx`.
5. Add the chip rail + filter drawer. New files
   `views/surface/chip-rail.tsx`, `views/surface/filter-drawer.tsx`.
6. Replace per-section detail panes with one
   `<DetailPane>` block in `views/surface/detail-pane.tsx` wrapping
   `Hub.rendererFor`, plus PAYLOAD / LINEAGE / METADATA sub-blocks.
7. Add `oneHopLineage()` selector to `graph-selectors.ts`. Pure
   function over the already-loaded `edgesById`. Wired into the
   detail pane on selection change.
8. Tab strip + per-tab default columns. `All` columns include pillar
   badge; per-pillar tabs drop it. New file
   `views/surface/tab-strip.tsx`.
9. Compose everything in a slimmed `surface.tsx` that orchestrates
   the children and persists tab + drawer state via the existing
   settings store.

Each step is independently testable against `surface.test.tsx`; v1
ships when `surface.test.tsx` is rewritten green plus a new
`surface-filter.test.tsx` covers the chip/drawer round-trip.
