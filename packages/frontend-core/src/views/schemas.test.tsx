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
  tombstone: null,
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

  it("groups namespace-aligned schemas (incl. codec-less CitedObjects) into the owning flavor", () => {
    // Production registers flavors under their full schema-id
    // namespace ("proxima-code"), so the namespace-fallback path
    // catches CitedObject / CitationMapping schemas that never carry
    // their own codec. Heading strips the `proxima-` prefix.
    const hub = createHub([]);
    hub.registerFlavor("proxima-code", (scope) => {
      const codec = {
        decode: () => null,
        encode: () => new Uint8Array(),
      };
      scope.registerCodec("proxima-code/code-chunk-v1", 1, codec);
    });

    render(() => (
      <GraphProvider
        store={store([
          schema("proxima-code/code-chunk-v1", "Fact", "proxima_code.code_chunk_v1"),
          schema("proxima-code/code-blob-v1", "CitedObject"),
          schema("proxima-code/code-blob-whole-v1", "CitationMapping"),
        ])}
      >
        <SchemasView hub={hub} />
      </GraphProvider>
    ));

    // All three render in the same flavor group (open by default).
    expect(screen.getByText("proxima-code/code-chunk-v1 v1")).toBeTruthy();
    expect(screen.getByText("proxima-code/code-blob-v1 v1")).toBeTruthy();
    expect(screen.getByText("proxima-code/code-blob-whole-v1 v1")).toBeTruthy();

    // Display label strips the `proxima-` prefix on the heading.
    expect(screen.getByRole("heading", { level: 2, name: "code" })).toBeTruthy();

    // No substrate group materialises when no schemas fell through.
    expect(screen.queryByRole("button", { name: /substrate/i })).toBeNull();
  });
});
