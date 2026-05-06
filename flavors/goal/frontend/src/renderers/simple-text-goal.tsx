export interface SimpleTextGoalPayload {
  text: string;
}

export function SimpleTextGoalRenderer(props: { payload: SimpleTextGoalPayload }) {
  return <p class="proxima-goal-text">{props.payload.text}</p>;
}
