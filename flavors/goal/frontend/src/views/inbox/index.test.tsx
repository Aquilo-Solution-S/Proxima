import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import type { GoalDraft, GoalRow } from "@proxima/core";
import { Inbox, type InboxEnv } from ".";

const goal = (): GoalRow => ({
  id: "00000000-0000-0000-0000-000000000001",
  schema_id: "proxima-goal/simple-text-v1",
  schema_version: 1,
  owner: {
    principal: { User: "00000000-0000-0000-0000-000000000000" },
    org_id: "00000000-0000-0000-0000-000000000000",
  },
  text: "test",
  state: "Proposed",
  parent_goal_ids: [],
  supersedes: null,
  payload: [],
});

const env = (writeGoal = vi.fn<InboxEnv["writeGoal"]>()): InboxEnv => ({
  proposedGoals: [
    {
      goal: goal(),
      payload: { text: "test" },
      evidence: [{ id: "a1", kind: "Abstraction" }],
    },
  ],
  writeGoal,
});

describe("goal inbox", () => {
  it("lists Proposed goals and offers Accept/Modify/Decline", () => {
    const { getByText } = render(() => <Inbox env={env()} />);
    expect(getByText("test")).toBeTruthy();
    expect(getByText("Accept")).toBeTruthy();
    expect(getByText("Modify")).toBeTruthy();
    expect(getByText("Decline")).toBeTruthy();
  });

  it("Accept calls GoalWrite with state=Active", async () => {
    const writeGoal = vi.fn<InboxEnv["writeGoal"]>().mockResolvedValue({});
    const { getByText } = render(() => <Inbox env={env(writeGoal)} />);
    fireEvent.click(getByText("Accept"));
    await waitFor(() => expect(writeGoal).toHaveBeenCalledOnce());
    const draft = writeGoal.mock.calls[0]![0] as GoalDraft;
    expect(draft.state).toBe("Active");
    expect(draft.supersedes_goal_id).toBe("00000000-0000-0000-0000-000000000001");
  });

  it("Decline calls GoalWrite with state=Rejected", async () => {
    const writeGoal = vi.fn<InboxEnv["writeGoal"]>().mockResolvedValue({});
    const { getByText } = render(() => <Inbox env={env(writeGoal)} />);
    fireEvent.click(getByText("Decline"));
    await waitFor(() => expect(writeGoal).toHaveBeenCalledOnce());
    expect((writeGoal.mock.calls[0]![0] as GoalDraft).state).toBe("Rejected");
  });

  it("Modify expands form and saves edited payload", async () => {
    const writeGoal = vi.fn<InboxEnv["writeGoal"]>().mockResolvedValue({});
    const { getByText, getByDisplayValue } = render(() => <Inbox env={env(writeGoal)} />);
    fireEvent.click(getByText("Modify"));
    fireEvent.input(getByDisplayValue("test"), { target: { value: "edited" } });
    fireEvent.click(getByText("Save"));
    await waitFor(() => expect(writeGoal).toHaveBeenCalledOnce());
    const draft = writeGoal.mock.calls[0]![0] as GoalDraft;
    expect(draft.state).toBe("Active");
    expect(draft.text).toBe("edited");
  });
});
