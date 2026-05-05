/*
 * PLACEHOLDER DATA STATE
 * This component renders the three-lane F→A→P traversal chrome with
 * empty-state placeholders. Data wiring (Query/Subscribe via hub) is
 * deferred to the next milestone.
 */

import { type Component } from "solid-js";
import { Mono } from "../primitives";
import type { Hub } from "../hub";

// ── Goal rail ───────────────────────────────────────────────────────────
const GoalRail: Component = () => (
  <div class="goal-rail">
    <div class="rail-head">
      <span class="rail-title">Goal DAG</span>
      <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
        supersession-only
      </Mono>
    </div>
    <div class="goal-list">
      <p class="proxima-dim">No goals</p>
    </div>
  </div>
);

// ── Event stream ───────────────────────────────────────────────────────
const EventStream: Component = () => (
  <div class="event-stream">
    <div class="stream-head">
      <span class="rail-title">Event stream</span>
      <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
        append-only
      </Mono>
    </div>
    <div class="stream-list">
      <p class="proxima-dim">No events</p>
    </div>
  </div>
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
export const FullSurface: Component<{ hub: Hub }> = () => (
  <div class="proxima-shell">
    <div class="surface-body">
      <GoalRail />
      <TraversalLanes />
      <EventStream />
    </div>
  </div>
);
