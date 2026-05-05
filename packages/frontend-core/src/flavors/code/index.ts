import type { FlavorScope } from "../../hub";
import { CodeView } from "./code-view";

export { CodeView };
export { ReposPanel } from "./repos-panel";

export function registerCode(scope: FlavorScope): void {
  scope.registerView({
    id: "code",
    label: "Code",
    component: CodeView,
  });
}
