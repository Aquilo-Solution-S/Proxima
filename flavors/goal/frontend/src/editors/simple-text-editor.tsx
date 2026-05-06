import type { GoalPayloadEditorComponent } from "@proxima/core/registry";
import type { SimpleTextGoalPayload } from "../renderers/simple-text-goal";

export const SimpleTextGoalEditor: GoalPayloadEditorComponent<SimpleTextGoalPayload> = (
  props,
) => (
  <label class="goal-editor-row">
    <span>Goal</span>
    <textarea
      rows={3}
      value={props.payload.text}
      onInput={(event) =>
        props.onChange({ ...props.payload, text: event.currentTarget.value })
      }
    />
  </label>
);

export const simpleTextGoalDefaults = (): SimpleTextGoalPayload => ({
  text: "",
});

export const simpleTextGoalToText = (
  payload: SimpleTextGoalPayload,
): string => payload.text;
