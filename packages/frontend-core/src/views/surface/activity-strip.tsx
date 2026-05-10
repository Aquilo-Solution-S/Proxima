import { Show, type Component } from "solid-js";

export type EngineState = "idle" | "waking" | "deciding" | "writing" | "error";

const formatRelative = (ms: number | null): string => {
  if (ms === null) return "—";
  const diff = Date.now() - ms;
  const min = Math.round(diff / 60_000);
  if (min < 1) return "now";
  if (min < 60) return `${min}m`;
  return `${Math.round(min / 60)}h`;
};

export const ActivityStrip: Component<{
  state: EngineState;
  lastWakeAtMs: number | null;
  activePersonalityCount: number;
  onToggleEventStream: () => void;
}> = (props) => (
  <button
    type="button"
    class="surface-activity-strip"
    aria-label="events"
    onClick={props.onToggleEventStream}
  >
    <span class={`surface-activity-strip__dot surface-activity-strip__dot--${props.state}`} />
    <span class="surface-activity-strip__state">{props.state}</span>
    <Show when={props.lastWakeAtMs !== null}>
      <span class="surface-activity-strip__sep">·</span>
      <span>last wake {formatRelative(props.lastWakeAtMs)} ago</span>
    </Show>
    <span class="surface-activity-strip__sep">·</span>
    <span>{props.activePersonalityCount} active personalities</span>
  </button>
);
