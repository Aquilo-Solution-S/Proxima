import "./surface.css";
/*
 * PLACEHOLDER DATA STATE
 * This component renders the three-lane F→A→P traversal chrome with
 * empty-state placeholders. Data wiring (Query/Subscribe via hub) is
 * deferred to the next milestone.
 */

import { Show, createSignal, type Component } from "solid-js";
import { Mono } from "../primitives";
import type { Hub } from "../hub";

// ── Goal rail ───────────────────────────────────────────────────────────
const GoalRail: Component<{
  collapsed: boolean;
  onToggle: () => void;
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
        <p class="proxima-dim">No goals</p>
      </div>
    </Show>
  </aside>
);

// ── Event stream ───────────────────────────────────────────────────────
const EventStream: Component<{
  collapsed: boolean;
  onToggle: () => void;
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
        <p class="proxima-dim">No events</p>
      </div>
    </Show>
  </aside>
);

// ── Traversal lanes (F→A→P) ───────────────────────────────────────────
const TraversalLanes: Component = () => (
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
        <p class="proxima-dim">No perspectives</p>
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
        <p class="proxima-dim">No abstractions</p>
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
        <p class="proxima-dim">No facts</p>
      </div>
    </div>
  </div>
);

// ── FullSurface ─────────────────────────────────────────────────────────
export const FullSurface: Component<{ hub: Hub }> = () => {
  const [goalsCollapsed, setGoalsCollapsed] = createSignal(false);
  const [eventsCollapsed, setEventsCollapsed] = createSignal(false);

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
          onToggle={() => setGoalsCollapsed((v) => !v)}
        />
        <TraversalLanes />
        <EventStream
          collapsed={eventsCollapsed()}
          onToggle={() => setEventsCollapsed((v) => !v)}
        />
      </div>
    </div>
  );
};
