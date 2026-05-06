import "./surface.css";

import {
  For,
  Show,
  createMemo,
  createSignal,
  type Component,
  type JSX,
} from "solid-js";
import type { ChangeEvent } from "../bindings";
import { SchemaTag, Mono } from "../primitives";
import { useGraph, type DecodedMemory } from "../graph-store";
import { useGraphFilter } from "../graph-filter-store";
import { filterGraphSnapshot } from "../graph-selectors";
import type { Hub } from "../hub";

// ── Goal rail ───────────────────────────────────────────────────────────
const GoalRail: Component<{
  collapsed: boolean;
  onToggle: () => void;
  goalCount: number;
}> = (props) => (
  <aside
    classList={{
      "goal-rail": true,
      "is-collapsed": props.collapsed,
    }}
  >
    <Show
      when={!props.collapsed}
      fallback={
        <button
          type="button"
          class="rail-collapsed-trigger"
          aria-label="Expand Goal DAG"
          aria-expanded="false"
          onClick={props.onToggle}
        >
          <span class="rail-collapse-icon is-closed" aria-hidden="true" />
          <span class="rail-collapsed-title">Goal DAG</span>
        </button>
      }
    >
      <div class="rail-head">
        <div class="rail-head-copy">
          <span class="rail-title">Goal DAG</span>
          <Mono style={{ "font-size": "9px", color: "var(--ink-50)" }}>
            supersession-only
          </Mono>
        </div>
        <button
          type="button"
          class="rail-toggle"
          aria-label="Collapse Goal DAG"
          aria-expanded="true"
          onClick={props.onToggle}
        >
          <span class="rail-collapse-icon is-left" aria-hidden="true" />
        </button>
      </div>
      <div class="goal-list">
        <p class="proxima-dim">
          {props.goalCount === 0
            ? "No goals"
            : `${props.goalCount} goal identities pending payload projection`}
        </p>
      </div>
    </Show>
  </aside>
);

// ── Event stream ───────────────────────────────────────────────────────
const EventStream: Component<{
  collapsed: boolean;
  onToggle: () => void;
  events: readonly ChangeEvent[];
}> = (props) => (
  <aside
    classList={{
      "event-stream": true,
      "is-collapsed": props.collapsed,
    }}
  >
    <Show
      when={!props.collapsed}
      fallback={
        <button
          type="button"
          class="rail-collapsed-trigger"
          aria-label="Expand Event stream"
          aria-expanded="false"
          onClick={props.onToggle}
        >
          <span class="rail-collapse-icon is-open" aria-hidden="true" />
          <span class="rail-collapsed-title">Event stream</span>
        </button>
      }
    >
      <div class="stream-head">
        <div class="rail-head-copy">
          <span class="rail-title">Event stream</span>
          <Mono style={{ "font-size": "9px", color: "var(--ink-50)" }}>
            append-only
          </Mono>
        </div>
        <button
          type="button"
          class="rail-toggle"
          aria-label="Collapse Event stream"
          aria-expanded="true"
          onClick={props.onToggle}
        >
          <span class="rail-collapse-icon is-right" aria-hidden="true" />
        </button>
      </div>
      <div class="stream-list">
        <Show
          when={props.events.length > 0}
          fallback={<p class="proxima-dim surface-empty">No events</p>}
        >
          <For each={props.events}>
            {(event) => (
              <div class="fact-row">
                <div class="fact-gutter">
                  <span class="fact-glyph">CE</span>
                </div>
                <div class="fact-body">
                  <div class="fact-row-head">
                    <Mono style={{ "font-size": "10px" }}>
                      {event.seq.slice(0, 8)}
                    </Mono>
                    <span class="proxima-dim">
                      {event.kind.EntityAppend !== undefined
                        ? event.kind.EntityAppend.entity_kind
                        : "Edge"}
                    </span>
                  </div>
                </div>
              </div>
            )}
          </For>
        </Show>
      </div>
    </Show>
  </aside>
);

// ── Traversal lanes (F→A→P) ───────────────────────────────────────────
const renderMemoryPayload = (
  memory: DecodedMemory,
  hub: Hub,
): JSX.Element | null => {
  const renderer = hub.rendererFor(
    memory.row.schema_id,
    memory.row.schema_version,
  );
  return renderer?.render({
    memory: memory.row,
    payload: memory.payload,
  }) ?? null;
};

const shortId = (id: string): string => id.slice(0, 8);

const MemoryCard: Component<{ memory: DecodedMemory; hub: Hub }> = (props) => {
  const rendered = (): JSX.Element | null =>
    renderMemoryPayload(props.memory, props.hub);
  return (
    <article
      class={props.memory.row.kind === "Perspective" ? "p-card" : "a-card"}
      title={props.memory.row.id}
    >
      <div class="card-head">
        <SchemaTag
          id={props.memory.row.schema_id}
          version={props.memory.row.schema_version}
        />
      </div>
      <Show
        keyed
        when={rendered()}
        fallback={
          <p class="prose prose-small">
            {props.memory.decodeError?.message ??
              `${props.memory.row.payload.length} payload bytes`}
          </p>
        }
      >
        {(node) => node}
      </Show>
      <div class="card-foot">
        <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
          {shortId(props.memory.row.id)}
        </Mono>
      </div>
    </article>
  );
};

const MemoryExplorer: Component<{
  hub: Hub;
  memories: DecodedMemory[];
  label: string;
  glyph: string;
}> = (props) => {
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const selectedMemory = createMemo(
    () =>
      props.memories.find((memory) => memory.row.id === selectedId()) ??
      props.memories[0] ??
      null,
  );

  return (
    <div class="memory-explorer">
      <div class="fact-list" role="listbox" aria-label={props.label}>
        <For each={props.memories}>
          {(memory) => (
            <button
              type="button"
              classList={{
                "fact-list-item": true,
                "is-selected": selectedMemory()?.row.id === memory.row.id,
              }}
              role="option"
              aria-selected={selectedMemory()?.row.id === memory.row.id}
              title={memory.row.schema_id}
              onClick={() => setSelectedId(memory.row.id)}
            >
              <span class="fact-list-glyph" aria-hidden="true">
                {props.glyph}
              </span>
              <span class="fact-list-copy">
                <span class="fact-list-schema">{memory.row.schema_id}</span>
                <span class="fact-list-meta">
                  {shortId(memory.row.id)} · {memory.row.payload.length} bytes
                </span>
              </span>
            </button>
          )}
        </For>
      </div>

      <Show keyed when={selectedMemory()}>
        {(memory) => {
          const rendered = (): JSX.Element | null =>
            renderMemoryPayload(memory, props.hub);
          return (
            <article class="fact-detail">
              <div class="fact-detail-head">
                <SchemaTag
                  id={memory.row.schema_id}
                  version={memory.row.schema_version}
                />
                <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
                  {shortId(memory.row.id)}
                </Mono>
              </div>
              <div class="fact-detail-body">
                <Show
                  keyed
                  when={rendered()}
                  fallback={
                    <p class="prose prose-small">
                      {memory.decodeError?.message ??
                        `${memory.row.payload.length} payload bytes`}
                    </p>
                  }
                >
                  {(node) => node}
                </Show>
              </div>
              <div class="card-foot">
                <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
                  {memory.row.id}
                </Mono>
              </div>
            </article>
          );
        }}
      </Show>
    </div>
  );
};

const LayerHeader: Component<{
  contentId: string;
  glyph: string;
  name: string;
  count: number;
  detail: string;
  collapsed: boolean;
  onToggle: () => void;
}> = (props) => (
  <button
    type="button"
    class="lane-toggle"
    aria-label={`${props.collapsed ? "Expand" : "Collapse"} ${
      props.name
    } section`}
    aria-expanded={!props.collapsed}
    aria-controls={props.contentId}
    onClick={props.onToggle}
  >
    <span class="lane-label">
      <span class="lane-letter">{props.glyph}</span>
      <span class="lane-meta">
        <span class="lane-name">{props.name}</span>
        <span class="lane-count">{props.count}</span>
        <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
          {props.detail}
        </Mono>
      </span>
    </span>
    <span
      classList={{
        "lane-collapse-icon": true,
        "is-collapsed": props.collapsed,
      }}
      aria-hidden="true"
    />
  </button>
);

const TraversalLanes: Component<{ hub: Hub; memories: DecodedMemory[] }> = (
  props,
) => {
  const [perspectivesCollapsed, setPerspectivesCollapsed] =
    createSignal(false);
  const [abstractionsCollapsed, setAbstractionsCollapsed] =
    createSignal(false);
  const [factsCollapsed, setFactsCollapsed] = createSignal(false);
  const facts = () => props.memories.filter((m) => m.row.kind === "Fact");
  const abstractions = () =>
    props.memories.filter((m) => m.row.kind === "Abstraction");
  const perspectives = () =>
    props.memories.filter((m) => m.row.kind === "Perspective");

  return (
    <div class="traversal">
      <div class="traversal-head">
        <span class="rail-title">F → A → P traversal</span>
        <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
          chain(f, P_active)
        </Mono>
      </div>

      {/* PERSPECTIVE LANE */}
      <section
        classList={{
          lane: true,
          "lane-p": true,
          "is-collapsed": perspectivesCollapsed(),
        }}
      >
        <LayerHeader
          contentId="surface-perspectives-content"
          glyph="P"
          name="Perspective"
          count={perspectives().length}
          detail="causal claim carrier"
          collapsed={perspectivesCollapsed()}
          onToggle={() => setPerspectivesCollapsed((v) => !v)}
        />
        <Show when={!perspectivesCollapsed()}>
          <div id="surface-perspectives-content" class="lane-content">
            <Show
              when={perspectives().length > 0}
              fallback={<p class="proxima-dim">No perspectives</p>}
            >
              <For each={perspectives()}>
                {(memory) => <MemoryCard memory={memory} hub={props.hub} />}
              </For>
            </Show>
          </div>
        </Show>
      </section>

      {/* P → A connector */}
      <div class="lane-connector">
        <Mono
          style={{
            "font-size": "9px",
            color: "var(--ink-40)",
            "line-height": "32px",
            "text-align": "center",
          }}
        >
          A → P (provenance)
        </Mono>
      </div>

      {/* ABSTRACTION LANE */}
      <section
        classList={{
          lane: true,
          "lane-a": true,
          "is-collapsed": abstractionsCollapsed(),
        }}
      >
        <LayerHeader
          contentId="surface-abstractions-content"
          glyph="A"
          name="Abstractions"
          count={abstractions().length}
          detail="authored prose + typed scaffolding"
          collapsed={abstractionsCollapsed()}
          onToggle={() => setAbstractionsCollapsed((v) => !v)}
        />
        <Show when={!abstractionsCollapsed()}>
          <div
            id="surface-abstractions-content"
            class="lane-content lane-content-row"
          >
            <Show
              when={abstractions().length > 0}
              fallback={<p class="proxima-dim">No abstractions</p>}
            >
              <MemoryExplorer
                hub={props.hub}
                memories={abstractions()}
                label="Abstractions"
                glyph="A"
              />
            </Show>
          </div>
        </Show>
      </section>

      {/* A → F connector */}
      <div class="lane-connector">
        <Mono
          style={{
            "font-size": "9px",
            color: "var(--ink-40)",
            "line-height": "32px",
            "text-align": "center",
          }}
        >
          F → A (provenance)
        </Mono>
      </div>

      {/* FACTS LANE */}
      <section
        classList={{
          lane: true,
          "lane-f": true,
          "is-collapsed": factsCollapsed(),
        }}
      >
        <LayerHeader
          contentId="surface-facts-content"
          glyph="F"
          name="Facts"
          count={facts().length}
          detail="source_batch - F→A is intra-batch"
          collapsed={factsCollapsed()}
          onToggle={() => setFactsCollapsed((v) => !v)}
        />
        <Show when={!factsCollapsed()}>
          <div
            id="surface-facts-content"
            class="lane-content lane-content-row"
          >
            <Show
              when={facts().length > 0}
              fallback={<p class="proxima-dim">No facts</p>}
            >
              <MemoryExplorer
                hub={props.hub}
                memories={facts()}
                label="Facts"
                glyph="F"
              />
            </Show>
          </div>
        </Show>
      </section>
    </div>
  );
};

// ── FullSurface ─────────────────────────────────────────────────────────
export const FullSurface: Component<{ hub: Hub }> = (props) => {
  const graph = useGraph();
  const filters = useGraphFilter();
  const [goalsCollapsed, setGoalsCollapsed] = createSignal(true);
  const [eventsCollapsed, setEventsCollapsed] = createSignal(true);
  const filtered = createMemo(() =>
    filterGraphSnapshot(graph.state(), filters.state(), props.hub),
  );
  const memories = () => filtered().memories;
  const events = () =>
    Array.from(graph.state().eventsBySeq.values()).sort((a, b) =>
      a.seq < b.seq ? 1 : -1,
    );
  const goalCount = () => filtered().goals.length;

  return (
    <div class="proxima-shell">
      <div
        classList={{
          "surface-body": true,
          "is-goals-collapsed": goalsCollapsed(),
          "is-events-collapsed": eventsCollapsed(),
        }}
      >
        <GoalRail
          collapsed={goalsCollapsed()}
          goalCount={goalCount()}
          onToggle={() => setGoalsCollapsed((v) => !v)}
        />
        <TraversalLanes hub={props.hub} memories={memories()} />
        <EventStream
          collapsed={eventsCollapsed()}
          events={events()}
          onToggle={() => setEventsCollapsed((v) => !v)}
        />
      </div>
    </div>
  );
};
