import type { ActionDefinition, CatalogAction } from "./types";

export function actionId(action: ActionDefinition) {
  return action.name ?? action.entrypoint;
}

export function actionHref(action: ActionDefinition) {
  return `#actions?action_id=${encodeURIComponent(actionId(action))}`;
}

export function resolveActionReference(
  actions: readonly CatalogAction[],
  reference: string,
) {
  const normalized = reference.replaceAll("\\", "/");
  return actions.find(
    (action) =>
      action.name === reference ||
      action.entrypoint === reference ||
      `${action.source.replaceAll("\\", "/")}::${action.entrypoint}` === normalized,
  );
}
