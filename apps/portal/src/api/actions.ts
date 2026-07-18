import type { ActionDefinition } from "../artifacts/types";
import type { ExecutionAggregate } from "./history";

export type RecentHealth = {
  window: number;
  sample_size: number;
  succeeded: number;
  failed: number;
  active: number;
  success_rate?: number | null;
  average_duration_ms?: number | null;
  p95_duration_ms?: number | null;
};

export type ActionDetailDto = {
  action_id: string;
  display_name: string;
  current_revision: string;
  definition: ActionDefinition;
  recent_health: RecentHealth;
  recent_executions: ExecutionAggregate[];
};

export type ObservedRevision = {
  revision: string;
  status: "current" | "observed";
  first_observed_at_unix_nanos?: string | null;
  last_observed_at_unix_nanos?: string | null;
  runtime?: string | null;
  execution_count: number;
  runtime_host_stream_count: number;
};

export type ObservedRevisionPage = {
  revisions: ObservedRevision[];
  execution_history_truncated: boolean;
  log_history_truncated: boolean;
};

export const actionsApi = {
  detail: (actionId: string) => requestJson<ActionDetailDto>(
    `/internal/actions/detail?action_id=${encodeURIComponent(actionId)}`,
  ),
  revisions: (actionId: string) => requestJson<ObservedRevisionPage>(
    `/internal/actions/revisions?action_id=${encodeURIComponent(actionId)}`,
  ),
};

async function requestJson<T>(url: string): Promise<T> {
  const response = await fetch(url);
  const body = await response.json() as T & { message?: string };
  if (!response.ok) throw new Error(body.message ?? `Request failed (${response.status})`);
  return body;
}
