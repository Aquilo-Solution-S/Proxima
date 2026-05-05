/*
 * PLACEHOLDER DATA STATE
 * This component renders the compact (768px) responsive variant with
 * empty-state placeholders. Data wiring (Query/Subscribe via hub) is
 * deferred to the next milestone.
 */

import { type Component } from "solid-js";
import { Mono } from "../primitives";
import type { Hub } from "../hub";

// ── Compact Event Stream ───────────────────────────────────────────────
const CompactEventStream: Component = () => (
  <div class="compact-f">
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

// ── CompactSurface ─────────────────────────────────────────────────────
export const CompactSurface: Component<{ hub: Hub }> = () => (
  <div class="proxima-shell compact">
    <div class="compact-body">
      {/* Active goal collapsed */}
      <div class="compact-goal">
        <div class="compact-goal-head">
          <span class="rail-title">Active goal</span>
          <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
            Goal DAG
          </Mono>
        </div>
        <p class="proxima-dim">No goals</p>
      </div>

      {/* P-only at top */}
      <div class="compact-p">
        <div class="lane-label">
          <span class="lane-letter">P</span>
          <span class="lane-name">Perspective</span>
        </div>
        <p class="proxima-dim">No perspectives</p>
      </div>

      {/* A summarised as horizontal strip */}
      <div class="compact-a">
        <div class="lane-label">
          <span class="lane-letter">A</span>
          <span class="lane-name">Abstractions</span>
          <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
            authored prose
          </Mono>
        </div>
        <p class="proxima-dim">No abstractions</p>
      </div>

      {/* F: live tail */}
      <CompactEventStream />
    </div>
  </div>
);
