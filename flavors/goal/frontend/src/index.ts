import { registerEdgeStyle, registerPayloadRenderer } from "@proxima/core/registry";
import { goalPayloadCodec, goalRenderers } from "./renderers/payload-renderers";

export { SimpleTextGoalRenderer } from "./renderers/simple-text-goal";
export { TaskGoalRenderer } from "./renderers/task-goal";
export { motivatedByEdgeStyle } from "./renderers/motivated-by-edge";

export function init(): void {
  for (const [schemaId, renderer] of Object.entries(goalRenderers)) {
    registerPayloadRenderer({
      schemaId,
      schemaVersion: 1,
      kind: "Goal",
      flavor: "proxima-goal",
      codec: goalPayloadCodec,
      renderer,
    });
  }
  registerEdgeStyle({
    relationId: "proxima-goal/motivated-by",
    style: {
      color: 0x8f7cf4,
      highlightColor: 0xd9d1ff,
      opacity: 0.38,
    },
  });
}
