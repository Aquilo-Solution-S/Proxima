import { Show, createSignal } from "solid-js";
import { ModifyForm, type EditableGoalPayload } from "./modify-form";
import type { ProposedGoal } from ".";

export function GoalProposalCard(props: {
  proposal: ProposedGoal;
  busy: boolean;
  onAccept(): void;
  onDecline(): void;
  onModify(payload: EditableGoalPayload): void;
}) {
  const [editing, setEditing] = createSignal(false);
  const payload = () => props.proposal.payload;
  const title = () => {
    const current = payload();
    return "text" in current ? current.text : current.title;
  };

  return (
    <article class="proxima-goal-card">
      <div>
        <h2>{title()}</h2>
        <p>{props.proposal.evidence.length} evidence item(s)</p>
      </div>
      <div class="proxima-goal-actions">
        <button type="button" disabled={props.busy} onClick={props.onAccept}>
          Accept
        </button>
        <button type="button" disabled={props.busy} onClick={() => setEditing((v) => !v)}>
          Modify
        </button>
        <button type="button" disabled={props.busy} onClick={props.onDecline}>
          Decline
        </button>
      </div>
      <Show when={editing()}>
        <ModifyForm
          payload={props.proposal.payload}
          onCancel={() => setEditing(false)}
          onSave={(next) => {
            setEditing(false);
            props.onModify(next);
          }}
        />
      </Show>
    </article>
  );
}
