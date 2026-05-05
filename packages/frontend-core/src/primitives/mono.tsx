import { Component, JSX } from "solid-js";

export const Mono: Component<{
  children?: JSX.Element;
  dim?: boolean;
  color?: string | null;
  style?: JSX.CSSProperties;
}> = (props) => (
  <span
    class="proxima-mono"
    style={{
      "font-family": "'JetBrains Mono', ui-monospace, monospace",
      "font-feature-settings": "'ss01', 'ss02', 'cv11'",
      color: props.color ?? (props.dim ? "var(--ink-50)" : "inherit"),
      "letter-spacing": "-0.01em",
      ...props.style,
    }}
  >
    {props.children}
  </span>
);
