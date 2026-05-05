import type { Component } from "solid-js";

export const PlaceholderView: Component<{ label: string }> = (props) => (
  <section class="proxima-view proxima-view-placeholder">
    <h1>{props.label}</h1>
    <p class="proxima-dim">Not yet implemented in this milestone.</p>
  </section>
);
