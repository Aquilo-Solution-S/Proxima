import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ActivityStrip } from "./activity-strip";

afterEach(cleanup);

describe("ActivityStrip", () => {
  it("renders state, last wake, and personality count", () => {
    render(() => (
      <ActivityStrip
        state="idle"
        lastWakeAtMs={Date.now() - 60_000}
        activePersonalityCount={2}
        onToggleEventStream={() => {}}
      />
    ));
    expect(screen.getByText(/idle/i)).not.toBeNull();
    expect(screen.getByText(/2 active/i)).not.toBeNull();
    expect(screen.getByText(/1m/)).not.toBeNull();
  });

  it("toggles the Event Stream drawer on click", () => {
    const onToggle = vi.fn();
    render(() => (
      <ActivityStrip
        state="idle"
        lastWakeAtMs={null}
        activePersonalityCount={0}
        onToggleEventStream={onToggle}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: /events/i }));
    expect(onToggle).toHaveBeenCalled();
  });
});
