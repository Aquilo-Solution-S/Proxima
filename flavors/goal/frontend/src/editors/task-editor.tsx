import type { GoalPayloadEditorComponent } from "@proxima/core/registry";
import type { TaskGoalPayload, TaskPriority } from "../renderers/task-goal";

const priorities: TaskPriority[] = ["Low", "Medium", "High"];

export const TaskGoalEditor: GoalPayloadEditorComponent<TaskGoalPayload> = (
  props,
) => (
  <>
    <label class="goal-editor-row">
      <span>Title</span>
      <input
        type="text"
        value={props.payload.title}
        onInput={(event) =>
          props.onChange({ ...props.payload, title: event.currentTarget.value })
        }
      />
    </label>
    <label class="goal-editor-row">
      <span>Priority</span>
      <select
        value={props.payload.priority ?? "Medium"}
        onChange={(event) =>
          props.onChange({
            ...props.payload,
            priority: event.currentTarget.value as TaskPriority,
          })
        }
      >
        {priorities.map((p) => (
          <option value={p}>{p}</option>
        ))}
      </select>
    </label>
    <label class="goal-editor-row">
      <span>Due (optional)</span>
      <input
        type="text"
        placeholder="2026-06-01"
        value={props.payload.due_at ?? ""}
        onInput={(event) =>
          props.onChange({
            ...props.payload,
            due_at: event.currentTarget.value === ""
              ? null
              : event.currentTarget.value,
          })
        }
      />
    </label>
  </>
);

export const taskGoalDefaults = (): TaskGoalPayload => ({
  title: "",
  priority: "Medium",
  due_at: null,
});

export const taskGoalToText = (payload: TaskGoalPayload): string =>
  payload.title;
