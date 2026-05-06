import { decode, encode } from "cbor-x";
import type { PayloadCodec, Renderer } from "@proxima/core/hub";
import { SimpleTextGoalRenderer, type SimpleTextGoalPayload } from "./simple-text-goal";
import { TaskGoalRenderer, type TaskGoalPayload } from "./task-goal";

export type GoalPayload = SimpleTextGoalPayload | TaskGoalPayload;

export const goalPayloadCodec: PayloadCodec<GoalPayload> = {
  decode: (bytes) => decode(bytes) as GoalPayload,
  encode: (value) => encode(value),
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
