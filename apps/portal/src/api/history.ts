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

export type Page<T> = {
  items: T[];
  next_cursor?: string | null;
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
  trigger: {
    type: string;
    schedule_id?: string;
    trigger_id?: string;
    source?: { type: string; schedule_id?: string; trigger_id?: string };
    [key: string]: unknown;
  };
  state: string;
  active_attempt_id?: string | null;
  created_at: unknown;
  updated_at: unknown;
  attempts: Array<{
    attempt: { attempt_id: string; attempt_number: number };
    state: string;
    outcome?: string | null;
    started_at?: unknown;
    finished_at?: unknown;
    result?: {
      invocation_result: {
        output?: unknown;
        error?: {
          code: string;
          message: string;
          retryable: boolean;
          details?: unknown;
        } | null;
      };
      duration: { secs: number; nanos: number };
    } | null;
  }>;
  cancellation_intent?: { requested_at: unknown } | null;
  terminal_state?: { state: string; attempt_id?: string | null; accepted_at: unknown } | null;
  data_refs: Record<string, string | null>;
};

export type ExecutionFilters = {
  action_id?: string;
  action_revision?: string;
  state?: string;
  trigger?: string;
  created_after_unix_ms?: string;
  created_before_unix_ms?: string;
  search?: string;
};

export type RunScheduleResponse = {
  execution_id: string;
  attempt_id?: string;
  attempt_number?: number;
  status: string;
  output?: unknown;
};

export const historyApi = {
  schedules: (cursor?: string, limit = 50) => {
    const query = pageParams(cursor, limit);
    return requestJson<Page<ScheduleRecord>>(`/internal/scheduler/schedules?${query}`);
  },
  schedule: (id: string) => requestJson<ScheduleRecord>(`/internal/scheduler/schedules/${encodeURIComponent(id)}`),
  triggers: (id: string, kind?: "scheduled" | "manual", cursor?: string, limit = 50) => {
    const query = pageParams(cursor, limit);
    if (kind) query.set("kind", kind);
    return requestJson<Page<ScheduleTrigger>>(`/internal/scheduler/schedules/${encodeURIComponent(id)}/triggers?${query}`);
  },
  run: (id: string) => requestJson<RunScheduleResponse>(`/internal/scheduler/schedules/${encodeURIComponent(id)}/run`, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" }),
  enable: (id: string) => requestJson<ScheduleRecord>(`/internal/scheduler/schedules/${encodeURIComponent(id)}/enable`, { method: "POST" }),
  disable: (id: string) => requestJson<ScheduleRecord>(`/internal/scheduler/schedules/${encodeURIComponent(id)}/disable`, { method: "POST" }),
  executions: (filters: ExecutionFilters, cursor?: string, limit = 50) => {
    const query = pageParams(cursor, limit);
    for (const [key, value] of Object.entries(filters)) if (value?.trim()) query.set(key, value);
    return requestJson<Page<ExecutionAggregate>>(`/internal/executions?${query}`);
  },
  execution: (id: string) => requestJson<ExecutionAggregate>(`/internal/executions/${encodeURIComponent(id)}`),
};

function pageParams(cursor: string | undefined, limit: number) {
  const query = new URLSearchParams({ limit: String(limit) });
  if (cursor) query.set("cursor", cursor);
  return query;
}

async function requestJson<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init);
  const body = await response.json() as T & { message?: string };
  if (!response.ok) throw new Error(body.message ?? `Request failed (${response.status})`);
  return body;
}
