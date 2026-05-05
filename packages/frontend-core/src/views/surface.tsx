import "./surface.css";
/*
 * PLACEHOLDER DATA STATE
 * This component renders the three-lane F→A→P traversal chrome with
 * empty-state placeholders. Data wiring (Query/Subscribe via hub) is
 * deferred to the next milestone.
 */

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
const MemoryCard: Component<{ memory: DecodedMemory; hub: Hub }> = (props) => {
  const rendered = (): JSX.Element | null => {
    const renderer = props.hub.rendererFor(
      props.memory.row.schema_id,
      props.memory.row.schema_version,
    );
    return renderer?.render({
      memory: props.memory.row,
      payload: props.memory.payload,
    }) ?? null;
  };
  return (
    <article class={props.memory.row.kind === "Perspective" ? "p-card" : "a-card"}>
      <div class="card-head">
        <SchemaTag
          id={props.memory.row.schema_id}
          version={props.memory.row.schema_version}
        />
      </div>
      <Show
        when={rendered()}
        fallback={
          <p class="prose prose-small">
            {props.memory.decodeError?.message ??
              `${props.memory.row.payload.length} payload bytes`}
          </p>
        }
      >
        {(node) => node()}
      </Show>
      <div class="card-foot">
        <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
          {props.memory.row.id}
        </Mono>
      </div>
    </article>
  );
};

const shortId = (id: string): string => id.slice(0, 8);

const FactExplorer: Component<{ hub: Hub; facts: DecodedMemory[] }> = (props) => {
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const selectedFact = createMemo(
    () =>
      props.facts.find((memory) => memory.row.id === selectedId()) ??
      props.facts[0] ??
      null,
  );
  const rendered = createMemo((): JSX.Element | null => {
    const fact = selectedFact();
    if (fact === null) {
      return null;
    }
    const renderer = props.hub.rendererFor(
      fact.row.schema_id,
      fact.row.schema_version,
    );
    return renderer?.render({
      memory: fact.row,
      payload: fact.payload,
    }) ?? null;
  });

  return (
    <div class="fact-explorer">
      <div class="fact-list" role="listbox" aria-label="Facts">
        <For each={props.facts}>
          {(memory) => (
            <button
              type="button"
              classList={{
                "fact-list-item": true,
                "is-selected": selectedFact()?.row.id === memory.row.id,
              }}
              role="option"
              aria-selected={selectedFact()?.row.id === memory.row.id}
              title={memory.row.schema_id}
              onClick={() => setSelectedId(memory.row.id)}
            >
              <span class="fact-list-glyph" aria-hidden="true">F</span>
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

      <Show when={selectedFact()}>
        {(fact) => (
          <article class="fact-detail">
            <div class="fact-detail-head">
              <SchemaTag
                id={fact().row.schema_id}
                version={fact().row.schema_version}
              />
              <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
                {shortId(fact().row.id)}
              </Mono>
            </div>
            <div class="fact-detail-body">
              <Show
                when={rendered()}
                fallback={
                  <p class="prose prose-small">
                    {fact().decodeError?.message ??
                      `${fact().row.payload.length} payload bytes`}
                  </p>
                }
              >
                {(node) => node()}
              </Show>
            </div>
            <div class="card-foot">
              <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
                {fact().row.id}
              </Mono>
            </div>
          </article>
        )}
      </Show>
    </div>
  );
};

const TraversalLanes: Component<{ hub: Hub; memories: DecodedMemory[] }> = (
  props,
) => {
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
    <div class="lane lane-p">
      <div class="lane-label">
        <span class="lane-letter">P</span>
        <div class="lane-meta">
          <span class="lane-name">Perspective</span>
          <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
            causal claim carrier
          </Mono>
        </div>
      </div>
      <div class="lane-content">
        <Show
          when={perspectives().length > 0}
          fallback={<p class="proxima-dim">No perspectives</p>}
        >
          <For each={perspectives()}>
            {(memory) => <MemoryCard memory={memory} hub={props.hub} />}
          </For>
        </Show>
      </div>
    </div>

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
    <div class="lane lane-a">
      <div class="lane-label">
        <span class="lane-letter">A</span>
        <div class="lane-meta">
          <span class="lane-name">Abstractions</span>
          <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
            authored prose + typed scaffolding
          </Mono>
        </div>
      </div>
      <div class="lane-content lane-content-row">
        <Show
          when={abstractions().length > 0}
          fallback={<p class="proxima-dim">No abstractions</p>}
        >
          <For each={abstractions()}>
            {(memory) => <MemoryCard memory={memory} hub={props.hub} />}
          </For>
        </Show>
      </div>
    </div>

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
    <div class="lane lane-f">
      <div class="lane-label">
        <span class="lane-letter">F</span>
        <div class="lane-meta">
          <span class="lane-name">Facts</span>
          <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
            source_batch — F→A is intra-batch
          </Mono>
        </div>
      </div>
      <div class="lane-content lane-content-row">
        <Show
          when={facts().length > 0}
          fallback={<p class="proxima-dim">No facts</p>}
        >
          <FactExplorer hub={props.hub} facts={facts()} />
        </Show>
      </div>
    </div>
  </div>
  );
};

// ── FullSurface ─────────────────────────────────────────────────────────
export const FullSurface: Component<{ hub: Hub }> = (props) => {
  const graph = useGraph();
  const [goalsCollapsed, setGoalsCollapsed] = createSignal(true);
  const [eventsCollapsed, setEventsCollapsed] = createSignal(true);
  const memories = () => Array.from(graph.state().memoriesById.values());
  const events = () =>
    Array.from(graph.state().eventsBySeq.values()).sort((a, b) =>
      a.seq < b.seq ? 1 : -1,
    );
  const goalCount = () => graph.state().goalsById.size;

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
