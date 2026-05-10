import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TabStrip } from "./tab-strip";

afterEach(cleanup);

describe("TabStrip", () => {
  it("renders five tabs with counts", () => {
    render(() => (
      <TabStrip
        active="All"
        counts={{ All: 2485, Fact: 2360, Abstraction: 118, Perspective: 7, Goal: 0 }}
        onChange={() => {}}
        onToggleFilters={() => {}}
      />
    ));
    expect(screen.getByRole("tab", { name: /All 2485/ })).not.toBeNull();
    expect(screen.getByRole("tab", { name: /F 2360/ })).not.toBeNull();
  });

  it("invokes onChange when a tab is clicked", () => {
    const onChange = vi.fn();
    render(() => (
      <TabStrip
        active="All"
        counts={{ All: 2485, Fact: 2360, Abstraction: 118, Perspective: 7, Goal: 0 }}
        onChange={onChange}
        onToggleFilters={() => {}}
      />
    ));
    fireEvent.click(screen.getByRole("tab", { name: /F 2360/ }));
    expect(onChange).toHaveBeenCalledWith("Fact");
  });

  it("invokes onToggleFilters when ⚙ Filters is clicked", () => {
    const onToggleFilters = vi.fn();
    render(() => (
      <TabStrip
        active="All"
        counts={{ All: 0, Fact: 0, Abstraction: 0, Perspective: 0, Goal: 0 }}
        onChange={() => {}}
        onToggleFilters={onToggleFilters}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: /filters/i }));
    expect(onToggleFilters).toHaveBeenCalled();
  });
});
