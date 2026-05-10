import { decode, encode } from "cbor-x";
import type { PayloadCodec, Renderer } from "@proxima/core/hub";
import { SimpleTextGoalRenderer, type SimpleTextGoalPayload } from "./simple-text-goal";
import { TaskGoalRenderer, type TaskGoalPayload } from "./task-goal";

export type GoalPayload = SimpleTextGoalPayload | TaskGoalPayload;

export interface GoalProposedPayload {
  goal_id: string;
  schema_id: string;
  title: string;
}

export interface GoalActivatedPayload {
  goal_id: string;
  schema_id: string;
  title: string;
  accepted_at: string;
  evidence_count: number;
}

export type GoalLifecyclePayload = GoalProposedPayload | GoalActivatedPayload;

export const goalPayloadCodec: PayloadCodec<GoalPayload> = {
  decode: (bytes) => decode(bytes) as GoalPayload,
  encode: (value) => encode(value),
};

export const goalLifecycleCodec: PayloadCodec<GoalLifecyclePayload> = {
  decode: (bytes) => decode(bytes) as GoalLifecyclePayload,
  encode: (value) => encode(value),
};

const Field = (props: { label: string; value: string | number }) => (
  <div class="proxima-goal-lifecycle-row">
    <span>{props.label}</span>
    <span>{props.value}</span>
  </div>
);

const GoalProposedRenderer = (props: {
  payload: GoalProposedPayload | null;
}) => {
  const payload = () => props.payload;
  return (
    <div class="proxima-goal-lifecycle">
      <strong>{payload()?.title ?? "Goal proposed"}</strong>
      <Field label="goal" value={payload()?.goal_id ?? "unknown"} />
      <Field label="schema" value={payload()?.schema_id ?? "unknown"} />
    </div>
  );
};

const GoalActivatedRenderer = (props: {
  payload: GoalActivatedPayload | null;
}) => {
  const payload = () => props.payload;
  return (
    <div class="proxima-goal-lifecycle">
      <strong>{payload()?.title ?? "Goal activated"}</strong>
      <Field label="goal" value={payload()?.goal_id ?? "unknown"} />
      <Field label="schema" value={payload()?.schema_id ?? "unknown"} />
      <Field label="accepted" value={payload()?.accepted_at ?? "unknown"} />
      <Field label="evidence" value={payload()?.evidence_count ?? "unknown"} />
    </div>
  );
};

export const goalRenderers: Record<string, Renderer<GoalPayload>> = {
  "proxima-goal/simple-text-v1": {
    render: (props) => (
      <SimpleTextGoalRenderer payload={props.payload as SimpleTextGoalPayload} />
    ),
  },
  "proxima-goal/task-v1": {
    render: (props) => <TaskGoalRenderer payload={props.payload as TaskGoalPayload} />,
  },
};

export const goalLifecycleRenderers: Record<
  string,
  Renderer<GoalLifecyclePayload>
> = {
  "proxima-goal/goal-proposed-v1": {
    render: (props) => (
      <GoalProposedRenderer
        payload={props.payload as GoalProposedPayload | null}
      />
    ),
  },
  "proxima-goal/goal-activated-v1": {
    render: (props) => (
      <GoalActivatedRenderer
        payload={props.payload as GoalActivatedPayload | null}
      />
    ),
  },
};
