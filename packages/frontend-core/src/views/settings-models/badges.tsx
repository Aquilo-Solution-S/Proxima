import { Show, type Component } from "solid-js";
import type { EmbedCaps } from "../../bindings";

export const EmbedCapsBadges: Component<{ caps: EmbedCaps }> = (props) => (
  <span class="proxima-caps-badges">
    <span class="proxima-caps-badge">dim={props.caps.dim}</span>
    <Show when={props.caps.matryoshka}>
      <span class="proxima-caps-badge">matryoshka</span>
    </Show>
  </span>
);
