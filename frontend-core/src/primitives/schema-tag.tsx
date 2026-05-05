import { Component } from "solid-js";
import { Mono } from "./mono";

export const SchemaTag: Component<{
  id: string;
  version?: number;
}> = (props) => (
  <Mono
    style={{
      "font-size": "10px",
      "text-transform": "none",
      color: "var(--ink-50)",
      border: "1px solid var(--rule)",
      padding: "1px 6px",
      "border-radius": "2px",
      "letter-spacing": "0.02em",
      "white-space": "nowrap",
    }}
  >
    {props.id}
    {typeof props.version === "number" ? ` v${props.version}` : ""}
  </Mono>
);
