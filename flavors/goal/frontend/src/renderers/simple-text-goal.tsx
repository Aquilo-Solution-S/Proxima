export interface SimpleTextGoalPayload {}

export function SimpleTextGoalRenderer(props: { payload: SimpleTextGoalPayload }) {
  void props.payload;
  return null;
}
