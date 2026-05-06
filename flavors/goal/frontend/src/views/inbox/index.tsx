import { decode, encode } from "cbor-x";
import { For, Show, createMemo, createSignal } from "solid-js";
import { commands, type GoalDraft, type GoalRow } from "@proxima/core";
import { useGraph } from "@proxima/core/graph-store";
import { GoalProposalCard } from "./card";
import type { EditableGoalPayload } from "./modify-form";

export interface ProposedGoal {
  goal: GoalRow;
  payload: EditableGoalPayload;
  evidence: Array<{ id: string; kind: "Fact" | "Abstraction" }>;
}

export interface InboxEnv {
  proposedGoals: ProposedGoal[];
  writeGoal(draft: GoalDraft): Promise<unknown>;
}

export function Inbox(props: { env?: InboxEnv }) {
  const graph = props.env === undefined ? useGraph() : null;
  const [busy, setBusy] = createSignal<string | null>(null);
  const proposed = createMemo<ProposedGoal[]>(() => {
    if (props.env !== undefined) return props.env.proposedGoals;
    const snapshot = graph!.state();
    return [...snapshot.goalsById.values()]
      .filter((goal) => goal.state === "Proposed")
      .map((goal) => ({
        goal,
        payload: decode(new Uint8Array(goal.payload)) as EditableGoalPayload,
        evidence: [],
      }));
  });

  const writeGoal = async (
    proposal: ProposedGoal,
    state: "Active" | "Rejected",
    payload: EditableGoalPayload = proposal.payload,
  ) => {
    setBusy(proposal.goal.id);
    try {
      const bytes = [...encode(payload)];
      const draft: GoalDraft = {
        owner: proposal.goal.owner,
        schema_id: proposal.goal.schema_id,
        schema_version: proposal.goal.schema_version,
        text: "text" in payload ? payload.text : payload.title,
        payload: bytes,
        state,
        parent_goal_ids: proposal.goal.parent_goal_ids,
        supersedes_goal_id: proposal.goal.id,
        authorship: "User",
        request_id: `goal-inbox:${state}:${proposal.goal.id}:${Date.now()}`,
      };
      if (props.env !== undefined) {
        await props.env.writeGoal(draft);
      } else {
        const result = await commands.goalWrite(draft);
        if (result.status === "error") throw result.error;
        await graph!.refresh();
      }
    } finally {
      setBusy(null);
    }
  };

  return (
    <section class="proxima-view proxima-goal-inbox">
      <h1>Inbox</h1>
      <Show
        when={proposed().length > 0}
        fallback={<p class="proxima-dim">No proposed goals.</p>}
      >
        <div class="proxima-goal-list">
          <For each={proposed()}>
            {(proposal) => (
              <GoalProposalCard
                proposal={proposal}
                busy={busy() === proposal.goal.id}
                onAccept={() => writeGoal(proposal, "Active")}
                onDecline={() => writeGoal(proposal, "Rejected")}
                onModify={(payload) => writeGoal(proposal, "Active", payload)}
              />
            )}
          </For>
        </div>
      </Show>
    </section>
  );
}
