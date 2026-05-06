import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { SimpleTextGoalRenderer } from "./simple-text-goal";
import { TaskGoalRenderer } from "./task-goal";

describe("goal payload renderers", () => {
  it("renders simple text goal body", () => {
    const { getByText } = render(() => (
      <SimpleTextGoalRenderer payload={{ text: "ship it" }} />
    ));
    expect(getByText("ship it")).toBeTruthy();
  });

  it("renders task goal fields", () => {
    const { getByText } = render(() => (
      <TaskGoalRenderer
        payload={{
          title: "Review proposal",
          due_at: "2026-05-07T10:00:00Z",
          priority: "High",
        }}
      />
    ));
    expect(getByText("Review proposal")).toBeTruthy();
    expect(getByText("High")).toBeTruthy();
    expect(getByText("2026-05-07T10:00:00Z")).toBeTruthy();
  });
});
