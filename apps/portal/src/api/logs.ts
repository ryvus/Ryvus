export type LogLossCause = "ingestion_overflow" | "provider_failure" | "retention_eviction";
export type LogCompleteness = "active" | "complete" | "incomplete" | "unknown";

export type LogLossRange = {
  first_sequence: string;
  last_sequence: string;
  cause: LogLossCause;
};

export type LogStreamSummary = {
  runtime_host_id: string;
  action_key_id: string;
  action_revision: string;
  runtime_language: string;
  started_at_unix_nanos: string;
  first_sequence?: string | null;
  last_sequence?: string | null;
  completeness: LogCompleteness;
  persisted_record_count: string;
  ingestion_dropped_count: string;
  provider_dropped_count: string;
  evicted_record_count: string;
  loss_ranges: LogLossRange[];
  ended_at_unix_nanos?: string | null;
  evicted: boolean;
  evicted_from?: LogCompleteness | null;
};

export type LogAttributeValue =
  | { type: "string"; value: string }
  | { type: "bool"; value: boolean }
  | { type: "i64"; value: string }
  | { type: "f64"; value: number }
  | { type: "string_array"; value: string[] }
  | { type: "bool_array"; value: boolean[] }
  | { type: "i64_array"; value: string[] }
  | { type: "f64_array"; value: number[] };

export type LogRecord = {
  timestamp_unix_nanos: string;
  observed_timestamp_unix_nanos: string;
  stream_sequence: string;
  runtime_host_id: string;
  action_key_id: string;
  action_revision: string;
  runtime_language: string;
  runtime_session_id?: string | null;
  correlation?: {
    execution_id: string;
    attempt_id: string;
    attempt_number: number;
  } | null;
  severity: string;
  message: string;
  attributes: Record<string, LogAttributeValue>;
  trace_id?: string | null;
  span_id?: string | null;
};

export type LogFilters = {
  action_key_id?: string;
  action_revision?: string;
  runtime_host_id?: string;
  execution_id?: string;
  attempt_id?: string;
  severity?: string;
  search?: string;
};

export type LogRecordFilters = Pick<LogFilters, "execution_id" | "attempt_id"> & {
  severity?: string;
  search?: string;
};

export type LogStreamPage = { streams: LogStreamSummary[]; next_cursor?: string | null };
export type LogRecordPage = { records: LogRecord[]; next_cursor?: string | null };
export type ProjectedLogRecordPage = {
  records: LogRecord[];
  older_cursor?: string | null;
  newer_cursor?: string | null;
  has_older: boolean;
  has_newer: boolean;
};

export const logsApi = {
  streams: (filters: LogFilters, cursor?: string, limit = 50) => {
    const query = queryParams(filters, cursor, limit);
    return requestJson<LogStreamPage>(`/internal/logs/streams?${query}`);
  },
  records: (runtimeHostId: string, filters: LogRecordFilters, cursor?: string, limit = 100) => {
    const query = queryParams(filters, cursor, limit);
    return requestJson<LogRecordPage>(`/internal/logs/streams/${encodeURIComponent(runtimeHostId)}/records?${query}`);
  },
  projectedRecords: (
    actionKeyId: string,
    actionRevision: string,
    filters: LogRecordFilters & { runtime_host_id?: string },
    cursor?: { direction: "older" | "newer"; value: string },
    limit = 100,
  ) => {
    const query = queryParams(
      { action_key_id: actionKeyId, action_revision: actionRevision, ...filters },
      undefined,
      limit,
    );
    if (cursor) query.set(`${cursor.direction}_cursor`, cursor.value);
    return requestJson<ProjectedLogRecordPage>(`/internal/logs/projected-records?${query}`);
  },
};

function queryParams(filters: LogFilters, cursor: string | undefined, limit: number) {
  const query = new URLSearchParams({ limit: String(limit) });
  for (const [key, value] of Object.entries(filters)) {
    if (value) query.set(key, value);
  }
  if (cursor) query.set("cursor", cursor);
  return query;
}

async function requestJson<T>(url: string): Promise<T> {
  const response = await fetch(url);
  const body = await response.json() as T & { message?: string };
  if (!response.ok) throw new Error(body.message ?? `Request failed (${response.status})`);
  return body;
}
