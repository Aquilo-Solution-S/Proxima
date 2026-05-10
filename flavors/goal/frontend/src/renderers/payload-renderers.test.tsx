import { render } from "@solidjs/testing-library";
import type { MemoryRow } from "@proxima/core";
import { createHub } from "@proxima/core/hub";
import { describe, expect, it } from "vitest";
import { init } from "../index";
import {
  goalLifecycleCodec,
  goalLifecycleRenderers,
  type GoalActivatedPayload,
} from "./payload-renderers";
import { SimpleTextGoalRenderer } from "./simple-text-goal";
import { TaskGoalRenderer } from "./task-goal";

const memory: MemoryRow = {
  id: "019dfa60-0000-7000-8000-000000000001",
  kind: "Fact",
  schema_id: "proxima-goal/goal-activated-v1",
  schema_version: 1,
  owner: {
    principal: { User: "00000000-0000-0000-0000-000000000000" },
    org_id: "00000000-0000-0000-0000-000000000000",
  },
  payload: [],
};

describe("goal payload renderers", () => {
  it("renders simple text goal body", () => {
    const { container } = render(() => (
      <SimpleTextGoalRenderer payload={{}} />
    ));
    expect(container.textContent).toBe("");
  });

  it("renders task goal fields", () => {
    const { getByText } = render(() => (
      <TaskGoalRenderer
        payload={{
          due_at: "2026-05-07T10:00:00Z",
          priority: "High",
        }}
      />
    ));
    expect(getByText("High")).toBeTruthy();
    expect(getByText("2026-05-07T10:00:00Z")).toBeTruthy();
  });

  it("decodes and renders goal activation facts", () => {
    const payload: GoalActivatedPayload = {
      goal_id: "019dfa60-0000-7000-8000-000000000002",
      schema_id: "proxima-goal/simple-text-v1",
      title: "Planner handoff",
      accepted_at: "2026-05-10T20:10:00Z",
      evidence_count: 2,
    };
    const decoded = goalLifecycleCodec.decode(goalLifecycleCodec.encode(payload));
    const renderer = goalLifecycleRenderers["proxima-goal/goal-activated-v1"];
    const { getByText } = render(() =>
      renderer.render({ memory, payload: decoded }),
    );

    expect(getByText("Planner handoff")).toBeTruthy();
    expect(getByText("2026-05-10T20:10:00Z")).toBeTruthy();
    expect(getByText("2")).toBeTruthy();
  });

  it("renders pending goal activation facts without decoded payloads", () => {
    const renderer = goalLifecycleRenderers["proxima-goal/goal-activated-v1"];
    const { getAllByText, getByText } = render(() =>
      renderer.render({ memory, payload: null as unknown as GoalActivatedPayload }),
    );

    expect(getByText("Goal activated")).toBeTruthy();
    expect(getAllByText("unknown").length).toBe(4);
  });

  it("registers goal lifecycle fact codecs at flavor init", () => {
    init();
    const hub = createHub([]);

    expect(hub.codecFor("proxima-goal/goal-activated-v1", 1)).not.toBeNull();
    expect(
      hub.rendererFor("proxima-goal/goal-activated-v1", 1, "Fact"),
    ).not.toBeNull();
  });
});
