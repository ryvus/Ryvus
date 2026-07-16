export type ScheduleRecord = {
  schedule_id: string;
  stable_schedule_key: string;
  display_name: string;
  current_revision: number;
  availability: "available" | "unavailable";
  enablement: "enabled" | "disabled";
  next_trigger_at?: { secs_since_epoch: number; nanos_since_epoch: number } | string | null;
  last_scheduled_trigger_at?: { secs_since_epoch: number; nanos_since_epoch: number } | string | null;
};

export type ScheduleTrigger = {
  trigger_id: string;
  schedule_id: string;
  schedule_revision: number;
  action_id: string;
  action_revision: string;
  kind: "scheduled" | "manual";
  status: "pending" | "claimed" | "execution_created" | "missed" | "failed";
  execution_id?: string | null;
  requested_by?: string | null;
  requested_at?: unknown;
  scheduled_for?: unknown;
  failure_summary?: string | null;
  created_at: unknown;
};

export type ExecutionAggregate = {
  execution_id: string;
  action_id: string;
  action_revision: string;
  trigger: { type: string; [key: string]: unknown };
  state: string;
  created_at: unknown;
  updated_at: unknown;
  attempts: Array<{
    attempt: { attempt_id: string; attempt_number: number };
    state: string;
    result?: {
      invocation_result: { output?: unknown; error?: unknown };
      events: Array<{ type?: string; message?: string; level?: string }>;
      duration: { secs: number; nanos: number } | unknown;
    } | null;
  }>;
  terminal_state?: { state: string } | null;
  data_refs: Record<string, string | null>;
};

export type RunScheduleResponse = {
  execution_id: string;
  attempt_id?: string;
  attempt_number?: number;
  status: string;
  output?: unknown;
};

export const historyApi = {
  schedules: () => requestJson<ScheduleRecord[]>("/internal/scheduler/schedules"),
  schedule: (id: string) => requestJson<ScheduleRecord>(`/internal/scheduler/schedules/${encodeURIComponent(id)}`),
  triggers: (id: string) => requestJson<ScheduleTrigger[]>(`/internal/scheduler/schedules/${encodeURIComponent(id)}/triggers`),
  run: (id: string) => requestJson<RunScheduleResponse>(`/internal/scheduler/schedules/${encodeURIComponent(id)}/run`, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" }),
  enable: (id: string) => requestJson<ScheduleRecord>(`/internal/scheduler/schedules/${encodeURIComponent(id)}/enable`, { method: "POST" }),
  disable: (id: string) => requestJson<ScheduleRecord>(`/internal/scheduler/schedules/${encodeURIComponent(id)}/disable`, { method: "POST" }),
  executions: (actionId?: string, actionRevision?: string) => {
    const query = new URLSearchParams();
    if (actionId) query.set("action_id", actionId);
    if (actionRevision) query.set("action_revision", actionRevision);
    const suffix = query.size ? `?${query}` : "";
    return requestJson<ExecutionAggregate[]>(`/internal/executions${suffix}`);
  },
  execution: (id: string) => requestJson<ExecutionAggregate>(`/internal/executions/${encodeURIComponent(id)}`),
};

async function requestJson<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init);
  const body = await response.json() as T & { message?: string };
  if (!response.ok) throw new Error(body.message ?? `Request failed (${response.status})`);
  return body;
}
