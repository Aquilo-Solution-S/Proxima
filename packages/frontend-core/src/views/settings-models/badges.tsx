import { For, Show, type Component } from "solid-js";
import type { EmbedCaps, LlmCaps } from "../../bindings";

// Caps badge helpers
export const LlmCapsBadges: Component<{ caps: LlmCaps }> = (props) => {
  const axes = (): string[] => {
    const out: string[] = [];
    if (props.caps.tool_use) out.push("tool_use");
    if (props.caps.json_mode) out.push("json_mode");
    if (props.caps.long_context) out.push("long_context");
    if (props.caps.vision) out.push("vision");
    return out;
  };
  return (
    <span class="proxima-caps-badges">
      <For each={axes()} fallback={<span class="proxima-dim">—</span>}>
        {(axis) => <span class="proxima-caps-badge">{axis}</span>}
      </For>
    </span>
  );
};

export const EmbedCapsBadges: Component<{ caps: EmbedCaps }> = (props) => (
  <span class="proxima-caps-badges">
    <span class="proxima-caps-badge">dim={props.caps.dim}</span>
    <Show when={props.caps.matryoshka}>
      <span class="proxima-caps-badge">matryoshka</span>
    </Show>
  </span>
);
