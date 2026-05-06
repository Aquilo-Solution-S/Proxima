import { createSignal } from "solid-js";
import type { SimpleTextGoalPayload } from "../../renderers/simple-text-goal";
import type { TaskGoalPayload } from "../../renderers/task-goal";

export type EditableGoalPayload = SimpleTextGoalPayload | TaskGoalPayload;

export function ModifyForm(props: {
  payload: EditableGoalPayload;
  onSave(payload: EditableGoalPayload): void;
  onCancel(): void;
}) {
  const [value, setValue] = createSignal(
    "text" in props.payload ? props.payload.text : props.payload.title,
  );
  const save = () => {
    if ("text" in props.payload) {
      props.onSave({ text: value() });
    } else {
      props.onSave({ ...props.payload, title: value() });
    }
  };

  return (
    <form
      class="proxima-goal-modify"
      onSubmit={(event) => {
        event.preventDefault();
        save();
      }}
    >
      <label>
        <span>Goal</span>
        <input value={value()} onInput={(event) => setValue(event.currentTarget.value)} />
      </label>
      <div>
        <button type="submit">Save</button>
        <button type="button" onClick={props.onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}
