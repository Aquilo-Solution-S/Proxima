import { cleanup, render, screen } from "@solidjs/testing-library";
import { encode } from "cbor-x";
import { afterEach, describe, expect, it } from "vitest";
import type { MemoryRow } from "@proxima/core";
import { clearRegistriesForTests } from "@proxima/core/registry";
import { createHub } from "@proxima/core/hub";
import { init } from "./index";

const owner = {
  principal: { User: "00000000-0000-0000-0000-000000000000" },
  org_id: "00000000-0000-0000-0000-000000000000",
};

const memory = (
  schemaId: string,
  kind: MemoryRow["kind"],
  payload: number[],
): MemoryRow => ({
  id: "019dfceb-03e2-7912-8f0f-ef97bb36bb58",
  kind,
  schema_id: schemaId,
  schema_version: 1,
  owner,
  payload,
});

describe("mcp payload renderers", () => {
  afterEach(() => {
    cleanup();
    clearRegistriesForTests();
  });

  it("renders agent-note title, body, and tags", () => {
    init();
    const hub = createHub([]);
    const payload = {
      note_id: "019dfceb-03e2-7912-8f0f-ef97bb36bb58",
      title: "mcp-bringup-test: streamable http listener",
      body: "Listener up at 127.0.0.1:31415/mcp.",
      tags: ["mcp", "bringup"],
    };
    const row = memory(
      "proxima-agent-memory/agent-note-v1",
      "Fact",
      Array.from(encode(payload)),
    );

    const decoded = hub
      .codecFor(row.schema_id, row.schema_version)
      ?.decode(new Uint8Array(row.payload));
    const renderer = hub.rendererFor(row.schema_id, row.schema_version);

    render(() => renderer?.render({ memory: row, payload: decoded }));

    expect(
      screen.getByText("mcp-bringup-test: streamable http listener"),
    ).toBeTruthy();
    expect(screen.getByText(/Listener up at 127\.0\.0\.1/)).toBeTruthy();
    expect(screen.getByText("mcp")).toBeTruthy();
    expect(screen.getByText("bringup")).toBeTruthy();
  });

  it("renders agent-derivation with sources count and model metadata", () => {
    init();
    const hub = createHub([]);
    const payload = {
      title: "MCP bringup status",
      body: "Listener verified end-to-end.",
      tags: [],
      source_memory_ids: [
        "019dfceb-03e2-7912-8f0f-ef97bb36bb58",
        "019dfceb-128e-7c23-b892-68f2119fbfa3",
      ],
      model_id: "claude-opus-4-7",
      client_name: "claude-code",
      client_version: "1.0.0",
    };
    const row = memory(
      "proxima-agent-memory/agent-derivation-v1",
      "Abstraction",
      Array.from(encode(payload)),
    );

    const decoded = hub
      .codecFor(row.schema_id, row.schema_version)
      ?.decode(new Uint8Array(row.payload));
    const renderer = hub.rendererFor(row.schema_id, row.schema_version);

    render(() => renderer?.render({ memory: row, payload: decoded }));

    expect(screen.getByText("MCP bringup status")).toBeTruthy();
    expect(screen.getByText("claude-opus-4-7")).toBeTruthy();
    expect(screen.getByText("2")).toBeTruthy();
  });

  it("renders agent-link confidence and reason", () => {
    init();
    const hub = createHub([]);
    const payload = { reason: "Same MCP bringup session.", confidence: 90 };
    const row = memory(
      "proxima-agent-memory/agent-link-v1",
      "Fact",
      Array.from(encode(payload)),
    );

    const decoded = hub
      .codecFor(row.schema_id, row.schema_version)
      ?.decode(new Uint8Array(row.payload));
    const renderer = hub.rendererFor(row.schema_id, row.schema_version);

    render(() => renderer?.render({ memory: row, payload: decoded }));

    expect(screen.getByText("Same MCP bringup session.")).toBeTruthy();
    expect(screen.getByText("90%")).toBeTruthy();
  });
});
