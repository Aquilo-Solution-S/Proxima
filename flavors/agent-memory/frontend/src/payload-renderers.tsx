import "./payload-renderers.css";

import { decode, encode } from "cbor-x";
import type { Component, JSX } from "solid-js";
import { For, Show } from "solid-js";
import type { MemoryRow } from "@proxima/core";
import type { PayloadCodec, Renderer } from "@proxima/core/hub";

type PayloadRecord = Record<string, unknown>;

export const agentMemoryPayloadCodec: PayloadCodec<unknown> = {
  decode(bytes: Uint8Array): unknown {
    return decode(bytes);
  },
  encode(value: unknown): Uint8Array {
    return encode(value);
  },
};

const isRecord = (value: unknown): value is PayloadRecord =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const asRecord = (value: unknown): PayloadRecord =>
  isRecord(value) ? value : {};

const asString = (value: unknown): string | null =>
  typeof value === "string" ? value : null;

const asNumber = (value: unknown): number | null =>
  typeof value === "number" && Number.isFinite(value) ? value : null;

const asStringList = (value: unknown): string[] =>
  Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string")
    : [];

const asArrayLength = (value: unknown): number =>
  Array.isArray(value) ? value.length : 0;

const Field: Component<{ label: string; children: JSX.Element }> = (props) => (
  <>
    <dt>{props.label}</dt>
    <dd>{props.children}</dd>
  </>
);

const PayloadShell: Component<{ title: string; children: JSX.Element }> = (
  props,
) => (
  <div class="agent-memory-payload">
    <div class="agent-memory-payload-title">{props.title}</div>
    {props.children}
  </div>
);

const PayloadGrid: Component<{ children: JSX.Element }> = (props) => (
  <dl class="payload-grid agent-memory-payload-grid">{props.children}</dl>
);

const Tags: Component<{ tags: string[] }> = (props) => (
  <Show when={props.tags.length > 0}>
    <div class="agent-memory-payload-tags">
      <For each={props.tags}>
        {(tag) => <span class="agent-memory-payload-tag">{tag}</span>}
      </For>
    </div>
  </Show>
);

const Body: Component<{ text: string }> = (props) => (
  <Show when={props.text.length > 0}>
    <p class="agent-memory-payload-body">{props.text}</p>
  </Show>
);

const Confidence: Component<{ value: number }> = (props) => {
  const pct = () => Math.max(0, Math.min(100, props.value));
  return (
    <div class="agent-memory-payload-confidence">
      <span>{pct()}%</span>
      <span class="agent-memory-payload-confidence-bar">
        <span
          class="agent-memory-payload-confidence-fill"
          style={{ width: `${pct()}%` }}
        />
      </span>
    </div>
  );
};

const renderAgentNote = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  const title = asString(p.title) ?? "untitled note";
  const body = asString(p.body) ?? "";
  const tags = asStringList(p.tags);
  return (
    <PayloadShell title={title}>
      <Body text={body} />
      <Tags tags={tags} />
      <Show when={asString(p.idempotency_key) !== null}>
        <PayloadGrid>
          <Field label="idempotency">{asString(p.idempotency_key) ?? ""}</Field>
        </PayloadGrid>
      </Show>
    </PayloadShell>
  );
};

const renderAgentDerivation = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  const title = asString(p.title) ?? "untitled derivation";
  const body = asString(p.body) ?? "";
  const tags = asStringList(p.tags);
  const sourceCount = asArrayLength(p.source_memory_ids);
  return (
    <PayloadShell title={title}>
      <Body text={body} />
      <Tags tags={tags} />
      <PayloadGrid>
        <Field label="model">{asString(p.model_id) ?? "unknown"}</Field>
        <Field label="client">
          {asString(p.client_name) ?? "unknown"}
          {" "}@{" "}
          {asString(p.client_version) ?? "unknown"}
        </Field>
        <Field label="sources">{sourceCount}</Field>
        <Show when={asString(p.idempotency_key) !== null}>
          <Field label="idempotency">{asString(p.idempotency_key) ?? ""}</Field>
        </Show>
      </PayloadGrid>
    </PayloadShell>
  );
};

const renderAgentLink = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  const reason = asString(p.reason) ?? "no reason recorded";
  const confidence = asNumber(p.confidence) ?? 0;
  return (
    <PayloadShell title="agent link">
      <Body text={reason} />
      <Confidence value={confidence} />
    </PayloadShell>
  );
};

const renderer = (
  render: (payload: unknown, memory: MemoryRow) => JSX.Element,
): Renderer<unknown> => ({
  render: (props) => render(props.payload, props.memory),
});

export const agentMemoryRenderers: Record<string, Renderer<unknown>> = {
  "proxima-agent-memory/agent-note-v1": renderer(renderAgentNote),
  "proxima-agent-memory/agent-derivation-v1": renderer(renderAgentDerivation),
  "proxima-agent-memory/agent-link-v1": renderer(renderAgentLink),
};
