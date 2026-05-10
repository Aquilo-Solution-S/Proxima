import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import { GraphFilterProvider, createGraphFilterStore } from "../../graph-filter-store";
import { FilterDrawer } from "./filter-drawer";

const FACETS = {
  flavors: ["proxima-code"],
  schemas: [
    { schemaId: "proxima-code/code-chunk-v1", flavor: "proxima-code" },
    { schemaId: "proxima-code/commit-summary-v1", flavor: "proxima-code" },
  ],
  authors: ["personality-rust", "personality-go"],
};

afterEach(cleanup);

describe("FilterDrawer", () => {
  it("toggles a schema checkbox and updates the store", () => {
    const store = createGraphFilterStore();
    render(() => (
      <GraphFilterProvider store={store}>
        <FilterDrawer open={true} onClose={() => {}} facets={FACETS} />
      </GraphFilterProvider>
    ));
    fireEvent.click(screen.getByLabelText(/code-chunk-v1/));
    expect(store.state().schemaIds.has("proxima-code/code-chunk-v1")).toBe(true);
  });

  it("Reset clears all facets", () => {
    const store = createGraphFilterStore();
    store.setAuthor("personality-rust", true);
    render(() => (
      <GraphFilterProvider store={store}>
        <FilterDrawer open={true} onClose={() => {}} facets={FACETS} />
      </GraphFilterProvider>
    ));
    fireEvent.click(screen.getByRole("button", { name: /reset/i }));
    expect(store.state().authoredBy.size).toBe(0);
  });

  it("does not render when closed", () => {
    render(() => (
      <GraphFilterProvider store={createGraphFilterStore()}>
        <FilterDrawer open={false} onClose={() => {}} facets={FACETS} />
      </GraphFilterProvider>
    ));
    expect(screen.queryByRole("button", { name: /reset/i })).toBeNull();
  });
});
