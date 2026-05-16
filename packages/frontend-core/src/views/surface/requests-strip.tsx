import { For, Show, createSignal, type Component } from "solid-js";
import type { GoalRow } from "../../bindings";

export interface RequestsStripProps {
  proposals: GoalRow[];
  onAccept: (proposal: GoalRow) => void;
  onDecline: (proposal: GoalRow) => void;
  pendingId: string | null;
}

export const RequestsStrip: Component<RequestsStripProps> = (props) => {
  const [expanded, setExpanded] = createSignal(true);

  return (
    <Show when={props.proposals.length > 0}>
      <section class="surface-requests" aria-label="Goal requests">
        <button
          type="button"
          class="surface-requests__head"
          aria-expanded={expanded()}
          onClick={() => setExpanded((v) => !v)}
        >
          <span class="surface-requests__caret" aria-hidden="true">
            {expanded() ? "▾" : "▸"}
          </span>
          <span class="surface-requests__label">Requests</span>
          <span class="surface-requests__count">{props.proposals.length}</span>
        </button>
        <Show when={expanded()}>
          <ul class="surface-requests__list" role="list">
            <For each={props.proposals}>
              {(proposal) => {
                const busy = (): boolean => props.pendingId === proposal.id;
                return (
                  <li class="surface-requests__row">
                    <div class="surface-requests__row-text">
                      <span class="surface-requests__title">
                        {proposal.title || "(untitled goal)"}
                      </span>
                      <span class="surface-requests__schema proxima-mono">
                        {proposal.schema_id}
                      </span>
                    </div>
                    <div class="surface-requests__row-actions">
                      <button
                        type="button"
                        class="surface-requests__btn surface-requests__btn--accept"
                        disabled={busy()}
                        onClick={() => props.onAccept(proposal)}
                      >
                        Accept
                      </button>
                      <button
                        type="button"
                        class="surface-requests__btn surface-requests__btn--decline"
                        disabled={busy()}
                        onClick={() => props.onDecline(proposal)}
                      >
                        {busy() ? "..." : "Decline"}
                      </button>
                    </div>
                  </li>
                );
              }}
            </For>
          </ul>
        </Show>
      </section>
    </Show>
  );
};
