import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SchemaInfo } from "./bindings";
import { FilterDialog } from "./filter-dialog";
import { GraphFilterProvider, createGraphFilterStore } from "./graph-filter-store";

const schemas: SchemaInfo[] = [
  {
    schema_id: "proxima-code/code-chunk-v1",
    schema_version: 1,
    kind: "Fact",
    filter_keys: [],
    sidecar_table: "proxima_code.code_chunk_v1",
    natural_key_columns: [],
  },
];

describe("FilterDialog", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("mutates global layer and search filters", () => {
    vi.useFakeTimers();
    const store = createGraphFilterStore();
    render(() => (
      <GraphFilterProvider store={store}>
        <FilterDialog open={true} schemas={schemas} flavors={["code"]} onClose={() => undefined} />
      </GraphFilterProvider>
    ));
    fireEvent.click(screen.getByRole("checkbox", { name: "Fact" }));
    fireEvent.input(screen.getByLabelText("Search"), { target: { value: "chunker" } });
    vi.advanceTimersByTime(120);
    expect(store.state().layers.has("Fact")).toBe(false);
    expect(store.state().search).toBe("chunker");
  });

  it("resets all filters", () => {
    const store = createGraphFilterStore();
    store.setLayer("Fact", false);
    render(() => (
      <GraphFilterProvider store={store}>
        <FilterDialog open={true} schemas={schemas} flavors={["code"]} onClose={() => undefined} />
      </GraphFilterProvider>
    ));
    fireEvent.click(screen.getByRole("button", { name: "Reset" }));
    expect(store.state().layers.has("Fact")).toBe(true);
  });

  it("cancels a pending debounced search update when reset is clicked", () => {
    vi.useFakeTimers();
    const store = createGraphFilterStore();
    render(() => (
      <GraphFilterProvider store={store}>
        <FilterDialog open={true} schemas={schemas} flavors={["code"]} onClose={() => undefined} />
      </GraphFilterProvider>
    ));
    fireEvent.input(screen.getByLabelText("Search"), { target: { value: "stale" } });
    fireEvent.click(screen.getByRole("button", { name: "Reset" }));
    vi.advanceTimersByTime(200);
    expect(store.state().search).toBe("");
  });
});
