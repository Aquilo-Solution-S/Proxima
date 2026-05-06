import type { GoalPayloadEditorComponent } from "@proxima/core/registry";
import type { SimpleTextGoalPayload } from "../renderers/simple-text-goal";

export const SimpleTextGoalEditor: GoalPayloadEditorComponent<SimpleTextGoalPayload> = (
  props,
) => {
  void props;
  return null;
};

export const simpleTextGoalDefaults = (): SimpleTextGoalPayload => ({});
