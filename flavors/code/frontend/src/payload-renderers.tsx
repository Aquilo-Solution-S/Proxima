import "./payload-renderers.css";

import type { Component, JSX } from "solid-js";
import type { MemoryRow } from "@proxima/core";
import type { Renderer } from "@proxima/core/hub";
import { highlightedCode } from "./code-highlight";

type PayloadRecord = Record<string, unknown>;

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

const byteArray = (value: unknown): Uint8Array | null => {
  if (value instanceof Uint8Array) return value;
  if (
    Array.isArray(value) &&
    value.every((item) => Number.isInteger(item) && item >= 0 && item <= 255)
  ) {
    return new Uint8Array(value);
  }
  return null;
};

const hex = (value: unknown): string | null => {
  const bytes = byteArray(value);
  if (bytes === null) return null;
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
};

const uuid = (value: unknown): string => {
  const text = asString(value);
  if (text !== null) return text;
  const raw = hex(value);
  if (raw === null || raw.length !== 32) return "unknown";
  return `${raw.slice(0, 8)}-${raw.slice(8, 12)}-${raw.slice(12, 16)}-${raw.slice(16, 20)}-${raw.slice(20)}`;
};

const shortSha = (value: unknown, len = 12): string =>
  asString(value)?.slice(0, len) ?? "unknown";

const stateText = (value: unknown): string => asString(value) ?? "unknown";

const optional = (value: unknown): string => asString(value) ?? "none";

const percent = (value: unknown): string => {
  const n = asNumber(value);
  return n === null ? "unknown" : `${Math.round(Math.max(0, Math.min(1, n)) * 100)}%`;
};

const range = (start: unknown, end: unknown): string => {
  const a = asNumber(start);
  const b = asNumber(end);
  return a === null || b === null ? "unknown" : `${a}-${b}`;
};

const Field: Component<{ label: string; children: JSX.Element }> = (props) => (
  <>
    <dt>{props.label}</dt>
    <dd>{props.children}</dd>
  </>
);

const PayloadShell: Component<{ title: string; children: JSX.Element }> = (
  props,
) => (
  <div class="code-payload">
    <div class="code-payload-title">{props.title}</div>
    {props.children}
  </div>
);

const PayloadGrid: Component<{ children: JSX.Element }> = (props) => (
  <dl class="payload-grid code-payload-grid">{props.children}</dl>
);

const CodeSnippet: Component<{ text: string; language: unknown }> = (props) => {
  const highlighted = () => highlightedCode(props.text, props.language);
  return (
    <pre class="code-payload-snippet">
      <code
        class={`hljs language-${highlighted().language}`}
        innerHTML={highlighted().html}
      />
    </pre>
  );
};

const renderCommit = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  const message = asString(p.message) ?? "";
  const firstLine = message.split(/\r?\n/, 1)[0] ?? "";
  return (
    <PayloadShell title={firstLine || shortSha(p.sha)}>
      <PayloadGrid>
        <Field label="sha">{shortSha(p.sha, 40)}</Field>
        <Field label="parents">{asStringList(p.parents).map((s) => s.slice(0, 12)).join(", ") || "none"}</Field>
        <Field label="author">{asString(p.author_name) ?? "unknown"} &lt;{asString(p.author_email) ?? "unknown"}&gt;</Field>
        <Field label="authored">{asString(p.author_time) ?? "unknown"}</Field>
        <Field label="committer">{asString(p.committer_name) ?? "unknown"} &lt;{asString(p.committer_email) ?? "unknown"}&gt;</Field>
        <Field label="committed">{asString(p.committer_time) ?? "unknown"}</Field>
        <Field label="repo">{uuid(p.repo_id)}</Field>
      </PayloadGrid>
    </PayloadShell>
  );
};

const renderFileRevision = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  const hash = hex(p.content_sha256);
  return (
    <PayloadShell title={asString(p.file_path) ?? "unknown file"}>
      <PayloadGrid>
        <Field label="state">{stateText(p.state)}</Field>
        <Field label="language">{optional(p.language)}</Field>
        <Field label="size">{asNumber(p.size_bytes) ?? "unknown"} bytes</Field>
        <Field label="commit">{shortSha(p.indexed_commit_sha, 40)}</Field>
        <Field label="content">{hash?.slice(0, 24) ?? "unknown"}</Field>
        <Field label="repo">{uuid(p.repo_id)}</Field>
      </PayloadGrid>
    </PayloadShell>
  );
};

const renderCodeChunk = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  const lineRange = range(p.line_range_start, p.line_range_end);
  return (
    <PayloadShell title={`${asString(p.file_path) ?? "unknown file"}:${lineRange}`}>
      <PayloadGrid>
        <Field label="state">{stateText(p.state)}</Field>
        <Field label="chunk">{asNumber(p.chunk_index) ?? "unknown"}</Field>
        <Field label="type">{asString(p.chunk_type) ?? "unknown"}</Field>
        <Field label="language">{optional(p.language)}</Field>
        <Field label="bytes">{range(p.byte_range_start, p.byte_range_end)}</Field>
        <Field label="repo">{uuid(p.repo_id)}</Field>
      </PayloadGrid>
      <CodeSnippet text={asString(p.text) ?? ""} language={p.language} />
    </PayloadShell>
  );
};

const renderCommitSummary = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  return (
    <PayloadShell title={shortSha(p.commit_sha, 12)}>
      <p class="code-payload-summary">{asString(p.summary) ?? "No summary"}</p>
      <PayloadGrid>
        <Field label="kind">{asString(p.change_kind) ?? "unknown"}</Field>
        <Field label="files">{asStringList(p.key_files).join(", ") || "none"}</Field>
        <Field label="repo">{uuid(p.repo_id)}</Field>
      </PayloadGrid>
    </PayloadShell>
  );
};

const renderDevelopmentPerspective = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  return (
    <PayloadShell title={asString(p.summary) ?? "Development perspective"}>
      <p class="code-payload-summary">{asString(p.pattern) ?? "No pattern"}</p>
      <PayloadGrid>
        <Field label="risk">{asString(p.risk) ?? "unknown"}</Field>
        <Field label="posture">{asString(p.recommended_posture) ?? "unknown"}</Field>
        <Field label="confidence">{percent(p.confidence)}</Field>
        <Field label="repo">{p.repo_id === null || p.repo_id === undefined ? "mixed" : uuid(p.repo_id)}</Field>
      </PayloadGrid>
    </PayloadShell>
  );
};

const renderExecutionRequest = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  return (
    <PayloadShell title={asString(p.title) ?? "Execution request"}>
      <p class="code-payload-summary">
        {asString(p.instructions) ?? "No instructions"}
      </p>
      <PayloadGrid>
        <Field label="request">{asString(p.request_key) ?? "unknown"}</Field>
        <Field label="repo">{uuid(p.repo_id)}</Field>
      </PayloadGrid>
    </PayloadShell>
  );
};

const renderPersonalitySelf = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  return (
    <PayloadShell title={asString(p.display_name) ?? "Personality"}>
      <p class="code-payload-summary">{asString(p.purpose) ?? "No purpose"}</p>
    </PayloadShell>
  );
};

const renderCalls = (payload: unknown): JSX.Element => {
  const p = asRecord(payload);
  return (
    <PayloadShell title={asString(p.callee_name) ?? "unknown callee"}>
      <PayloadGrid>
        <Field label="callsite">{range(p.callsite_byte_start, p.callsite_byte_end)}</Field>
        <Field label="dynamic">{p.is_dynamic === true ? "yes" : "no"}</Field>
      </PayloadGrid>
    </PayloadShell>
  );
};

const renderer = (
  render: (payload: unknown, memory: MemoryRow) => JSX.Element,
): Renderer<unknown> => ({
  render: (props) => render(props.payload, props.memory),
});

export const codeRenderers: Record<string, Renderer<unknown>> = {
  "proxima-code/commit-v1": renderer(renderCommit),
  "proxima-code/file-revision-v1": renderer(renderFileRevision),
  "proxima-code/code-chunk-v1": renderer(renderCodeChunk),
  "proxima-code/commit-summary-v1": renderer(renderCommitSummary),
  "proxima-code/development-perspective-v1": renderer(renderDevelopmentPerspective),
  "proxima-code/execution-request-v1": renderer(renderExecutionRequest),
  "proxima-code/calls": renderer(renderCalls),
  "proxima-code/engineer-self-v1": renderer(renderPersonalitySelf),
  "proxima-code/commit-summarizer-self-v1": renderer(renderPersonalitySelf),
};
