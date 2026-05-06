import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import type { SchemaInfo } from "../bindings";
import {
  GraphProvider,
  sentinelOwner,
  type GraphSnapshot,
  type GraphStore,
} from "../graph-store";
import { createHub } from "../hub";
import { SchemasView } from "./schemas";

const schema = (
  schemaId: string,
  kind: SchemaInfo["kind"],
  sidecarTable: string | null = null,
): SchemaInfo => ({
  schema_id: schemaId,
  schema_version: 1,
  kind,
  sidecar_table: sidecarTable,
  natural_key_columns: [],
  filter_keys: [],
});

const snapshot = (schemas: SchemaInfo[]): GraphSnapshot => ({
  owner: sentinelOwner(),
  schemas,
  memoriesById: new Map(),
  goalsById: new Map(),
  edgesById: new Map(),
  eventsBySeq: new Map(),
  pendingHydration: new Map(),
  decodeErrorsByEntity: new Map(),
  streamStatus: "live",
  seqHighWater: null,
});

const store = (schemas: SchemaInfo[]): GraphStore => ({
  state: () => snapshot(schemas),
  refresh: () => Promise.resolve(),
});

const hubWithCode = () => {
  const hub = createHub([]);
  hub.registerFlavor("code", (scope) => {
    const codec = {
      decode: () => null,
      encode: () => new Uint8Array(),
    };
    scope.registerCodec("code/fact-v1", 1, codec);
    scope.registerCodec("code/summary-v1", 1, codec);
  });
  return hub;
};

describe("SchemasView", () => {
  afterEach(() => cleanup());

  it("collapses substrate by default and filters a flavor by kind", () => {
    render(() => (
      <GraphProvider
        store={store([
          schema("code/fact-v1", "Fact", "code.fact_v1"),
          schema("code/summary-v1", "Abstraction", "code.summary_v1"),
          schema("substrate/cited-v1", "CitedObject"),
        ])}
      >
        <SchemasView hub={hubWithCode()} />
      </GraphProvider>
    ));

    expect(screen.getByText("code/fact-v1 v1")).toBeTruthy();
    expect(screen.queryByText("substrate/cited-v1 v1")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Fact 1" }));

    expect(screen.getByText("code/fact-v1 v1")).toBeTruthy();
    expect(screen.queryByText("code/summary-v1 v1")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /substrate/i }));

    expect(screen.getByText("substrate/cited-v1 v1")).toBeTruthy();
  });
});
