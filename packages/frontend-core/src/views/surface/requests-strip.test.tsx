import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { GoalRow } from "../../bindings";
import { RequestsStrip } from "./requests-strip";

afterEach(cleanup);

const proposal = (id: string, title: string): GoalRow => ({
  id,
  schema_id: "proxima-goal/simple-text-v1",
  schema_version: 1,
  owner: { principal: { User: "alice" }, org_id: "org-1" },
  title,
  text: "",
  state: "Proposed",
  parent_goal_ids: [],
  supersedes: null,
  payload: [],
});

describe("RequestsStrip", () => {
  it("renders nothing when there are no proposals", () => {
    render(() => (
      <RequestsStrip
        proposals={[]}
        pendingId={null}
        onAccept={vi.fn()}
        onDecline={vi.fn()}
      />
    ));
    expect(screen.queryByRole("button", { name: /accept/i })).toBeNull();
    expect(screen.queryByText(/requests/i)).toBeNull();
  });

  it("shows count and proposals when expanded", () => {
    render(() => (
      <RequestsStrip
        proposals={[proposal("g1", "Migrate enums"), proposal("g2", "CI sweep")]}
        pendingId={null}
        onAccept={vi.fn()}
        onDecline={vi.fn()}
      />
    ));
    expect(screen.getByText("2")).toBeTruthy();
    expect(screen.getByText("Migrate enums")).toBeTruthy();
    expect(screen.getByText("CI sweep")).toBeTruthy();
  });

  it("collapses and re-expands on header click", () => {
    render(() => (
      <RequestsStrip
        proposals={[proposal("g1", "Hidden when collapsed")]}
        pendingId={null}
        onAccept={vi.fn()}
        onDecline={vi.fn()}
      />
    ));
    expect(screen.getByText("Hidden when collapsed")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { expanded: true }));
    expect(screen.queryByText("Hidden when collapsed")).toBeNull();
    fireEvent.click(screen.getByRole("button", { expanded: false }));
    expect(screen.getByText("Hidden when collapsed")).toBeTruthy();
  });

  it("calls onAccept and onDecline with the proposal", () => {
    const onAccept = vi.fn();
    const onDecline = vi.fn();
    const g = proposal("g1", "Pick me");
    render(() => (
      <RequestsStrip
        proposals={[g]}
        pendingId={null}
        onAccept={onAccept}
        onDecline={onDecline}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: /accept/i }));
    expect(onAccept).toHaveBeenCalledWith(g);
    fireEvent.click(screen.getByRole("button", { name: /decline/i }));
    expect(onDecline).toHaveBeenCalledWith(g);
  });

  it("disables both buttons for the row matching pendingId", () => {
    render(() => (
      <RequestsStrip
        proposals={[proposal("g1", "Busy row"), proposal("g2", "Idle row")]}
        pendingId="g1"
        onAccept={vi.fn()}
        onDecline={vi.fn()}
      />
    ));
    const buttons = screen.getAllByRole("button");
    const busyAccept = buttons.find(
      (b) =>
        b.textContent?.toLowerCase().includes("accept") &&
        b.closest("li")?.textContent?.includes("Busy row"),
    ) as HTMLButtonElement;
    const idleAccept = buttons.find(
      (b) =>
        b.textContent?.toLowerCase().includes("accept") &&
        b.closest("li")?.textContent?.includes("Idle row"),
    ) as HTMLButtonElement;
    expect(busyAccept.disabled).toBe(true);
    expect(idleAccept.disabled).toBe(false);
  });
});
