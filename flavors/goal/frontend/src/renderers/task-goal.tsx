export type TaskPriority = "Low" | "Medium" | "High";

export interface TaskGoalPayload {
  title: string;
  due_at?: string | null;
  priority?: TaskPriority | null;
}

export function TaskGoalRenderer(props: { payload: TaskGoalPayload }) {
  return (
    <div class="proxima-goal-task">
      <strong>{props.payload.title}</strong>
      <span>{props.payload.priority ?? "Medium"}</span>
      {props.payload.due_at ? <time>{props.payload.due_at}</time> : null}
    </div>
  );
}
