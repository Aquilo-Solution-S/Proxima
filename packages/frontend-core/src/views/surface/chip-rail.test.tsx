// chip-rail.test.tsx
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import {
  GraphFilterProvider,
  createGraphFilterStore,
} from "../../graph-filter-store";
import { ChipRail } from "./chip-rail";

afterEach(cleanup);

const renderWithStore = (
  setup: (store: ReturnType<typeof createGraphFilterStore>) => void,
) => {
  const store = createGraphFilterStore();
  setup(store);
  render(() => (
    <GraphFilterProvider store={store}>
      <ChipRail flavors={["proxima-code"]} />
    </GraphFilterProvider>
  ));
  return store;
};

describe("ChipRail", () => {
  it("renders a chip for each active facet", () => {
    renderWithStore((store) => {
      store.setSchema("proxima-code/code-chunk-v1", true);
      store.setAuthor("personality-rust", true);
    });
    expect(screen.getByText(/code-chunk-v1/)).toBeTruthy();
    expect(screen.getByText(/personality-rust/)).toBeTruthy();
  });

  it("removes the chip when ✕ is clicked", () => {
    const store = renderWithStore((s) => {
      s.setSchema("proxima-code/code-chunk-v1", true);
    });
    const remove = screen.getByLabelText(/remove schema chip/i);
    fireEvent.click(remove);
    expect(screen.queryByText(/code-chunk-v1/)).toBeNull();
    expect(store.state().schemaIds.size).toBe(0);
  });

  it("renders nothing when no facets are active", () => {
    renderWithStore(() => {});
    expect(screen.queryByRole("listitem")).toBeNull();
  });
});
