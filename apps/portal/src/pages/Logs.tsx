import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { historyApi, type ExecutionAggregate } from "../api/history";
import {
  logsApi,
  type LogCompleteness,
  type LogFilters,
  type LogLossCause,
  type LogLossRange,
  type LogRecord,
  type LogRecordFilters,
  type LogStreamSummary,
} from "../api/logs";
import { Badge, Button, EmptyState, Page, Panel, cn } from "../components/ui";
import { ActionRevisionLogs } from "./ActionRevisionLogs";

export type LogViewContext = {
  actionId?: string;
  actionRevision?: string;
  executionId?: string;
  attemptId?: string;
  runtimeHostId?: string;
  withinAction?: boolean;
};

type LogGroupMode = "runtime_host" | "execution" | "time";

export function Logs({ context }: { context?: LogViewContext }) {
  if (context?.withinAction && context.actionId) return <ActionRevisionLogs context={context} />;
  return <StreamLogs context={context} />;
}

function StreamLogs({ context }: { context?: LogViewContext }) {
  const [hash, setHash] = useState(window.location.hash);
  useEffect(() => {
    const update = () => setHash(window.location.hash);
    window.addEventListener("hashchange", update);
    return () => window.removeEventListener("hashchange", update);
  }, []);

  const params = new URLSearchParams(hash.split("?")[1] ?? "");
  const withinAction = Boolean(context?.withinAction && context.actionId);
  const filters = logFilters(params, context);
  const groupMode = logGroupMode(params);
  const runtimeHostId = filters.runtime_host_id;
  const execution = useQuery({
    queryKey: ["execution", filters.execution_id],
    queryFn: () => historyApi.execution(filters.execution_id!),
    enabled: Boolean(filters.execution_id),
  });
  const authoritativeAttemptId = activeAttemptId(execution.data);
  const activeRuntimeHosts = useQuery({
    queryKey: ["active-log-stream-hosts", filters.execution_id, authoritativeAttemptId],
    queryFn: () => loadAttemptRuntimeHosts(filters.execution_id!, authoritativeAttemptId!),
    enabled: Boolean(filters.execution_id && !filters.attempt_id && authoritativeAttemptId),
  });
  const streams = useInfiniteQuery({
    queryKey: ["log-streams", filters],
    queryFn: ({ pageParam }) => logsApi.streams(filters, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
  });
  const records = useInfiniteQuery({
    queryKey: ["log-records", runtimeHostId, filters.execution_id, filters.attempt_id, filters.severity, filters.search],
    queryFn: ({ pageParam }) => logsApi.records(runtimeHostId!, recordFilters(filters), pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: Boolean(runtimeHostId),
  });
  const streamItems = streams.data?.pages.flatMap((page) => page.streams) ?? [];
  const selectedStream = streamItems.find((stream) => stream.runtime_host_id === runtimeHostId);
  const recordItems = records.data?.pages.flatMap((page) => page.records) ?? [];

  const content = (
    <>
      <LogFiltersForm params={params} context={context} groupMode={groupMode} />
      {groupMode === "execution" && !filters.execution_id && (
        <p className="text-sm text-amber-200">Set an exact execution ID to group matching Runtime Host streams by execution.</p>
      )}
      {groupMode === "time" && (
        <p className="text-xs text-slate-500">Stream summaries are ordered by start time. Records remain sequence-ordered within the selected Runtime Host.</p>
      )}
      {execution.isError && filters.execution_id && (
        <p className="text-sm text-amber-200">Execution state unavailable; active streams are shown as unknown.</p>
      )}
      {streams.isError && !streams.data ? (
        <EmptyState title="Log history unavailable" message={errorMessage(streams.error)} />
      ) : !streams.data ? (
        <EmptyState title="Loading log streams" message="Reading Runtime Host streams." />
      ) : streamItems.length === 0 ? (
        <EmptyState title="No logs" message="No Runtime Host streams match these exact filters." />
      ) : (
        <div className="grid gap-4 xl:grid-cols-[minmax(280px,380px)_minmax(0,1fr)]">
          <div className="grid content-start gap-2">
            {groupMode === "execution" && filters.execution_id && (
              <div className="mb-1 border-l-2 border-cyan-300/40 pl-3">
                <p className="font-mono text-[10px] font-bold uppercase text-slate-600">Execution</p>
                <p className="truncate font-mono text-xs text-cyan-200" title={filters.execution_id}>{filters.execution_id}</p>
              </div>
            )}
            {streamItems.map((stream) => (
              <StreamCard
                key={stream.runtime_host_id}
                stream={stream}
                selected={stream.runtime_host_id === runtimeHostId}
                params={params}
                context={context}
                completeness={projectLogCompleteness(
                  stream,
                  execution.data,
                  filters.execution_id,
                  filters.attempt_id,
                  activeRuntimeHosts.data,
                )}
              />
            ))}
            {streams.isError && (
              <div role="alert" className="grid gap-2 rounded-md border border-red-400/20 bg-red-500/[0.05] p-3 text-sm text-red-200">
                <span>Could not load more Runtime Host streams: {errorMessage(streams.error)}</span>
                <Button type="button" className="justify-self-start" onClick={() => void (streams.hasNextPage ? streams.fetchNextPage() : streams.refetch())} disabled={streams.isFetching}>
                  Retry
                </Button>
              </div>
            )}
            {streams.hasNextPage && (
              <Button type="button" className="justify-self-start" onClick={() => void streams.fetchNextPage()} disabled={streams.isFetchingNextPage}>
                {streams.isFetchingNextPage ? "Loading…" : "Load more"}
              </Button>
            )}
          </div>
          <RecordPanel
            stream={selectedStream}
            records={recordItems}
            query={records}
            context={context}
          />
        </div>
      )}
    </>
  );

  if (withinAction) return content;
  return (
    <Page
      eyebrow="Runtime observability"
      title="Logs"
      actions={filters.execution_id && (
        <a className="font-mono text-xs text-cyan-300 hover:text-cyan-100" href={`#execution-preview?id=${encodeURIComponent(filters.execution_id)}`}>
          Execution {filters.execution_id}
        </a>
      )}
    >
      {content}
    </Page>
  );
}

function LogFiltersForm({ params, context, groupMode }: { params: URLSearchParams; context?: LogViewContext; groupMode: LogGroupMode }) {
  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const next = new URLSearchParams(params);
    const revisionParameter = context?.withinAction ? "revision" : "action_revision";
    const currentAction = params.get("action_key_id") ?? "";
    const currentRevision = params.get(revisionParameter) ?? context?.actionRevision ?? "";
    const currentExecution = params.get("execution_id") ?? context?.executionId ?? "";
    const currentAttempt = params.get("attempt_id") ?? context?.attemptId ?? "";
    const action = String(form.get("action_key_id") ?? "").trim();
    const revision = String(form.get("action_revision") ?? "").trim();
    const execution = String(form.get("execution_id") ?? "").trim();
    const attempt = String(form.get("attempt_id") ?? "").trim();
    for (const name of ["action_key_id", "action_revision", "execution_id", "attempt_id", "runtime_host_id", "severity", "search", "group"] as const) {
      if (name === "action_key_id" && context?.withinAction) continue;
      const value = String(form.get(name) ?? "").trim();
      const parameter = name === "action_revision" ? revisionParameter : name;
      value ? next.set(parameter, value) : next.delete(parameter);
    }
    if (!context?.withinAction && action !== currentAction) {
      next.delete("action_revision");
      next.delete("execution_id");
      next.delete("attempt_id");
      next.delete("runtime_host_id");
    } else if (revision !== currentRevision) {
      next.delete("execution_id");
      next.delete("attempt_id");
      next.delete("runtime_host_id");
    } else if (execution !== currentExecution) {
      next.delete("attempt_id");
      next.delete("runtime_host_id");
    } else if (attempt !== currentAttempt) {
      next.delete("runtime_host_id");
    }
    if (context?.withinAction) next.delete("action_revision");
    navigateLogs(next, context);
  }

  const cleared = baseLogParams(context);

  return (
    <Panel className="p-3">
      <form key={params.toString()} className="grid gap-3 md:grid-cols-2 xl:grid-cols-4 xl:items-end" onSubmit={submit}>
        {!context?.withinAction && <label className="grid gap-1 font-mono text-[11px] font-bold uppercase text-slate-500">
          Action key
          <input name="action_key_id" defaultValue={params.get("action_key_id") ?? ""} placeholder="Exact action key" />
        </label>}
        <label className="grid gap-1 font-mono text-[11px] font-bold uppercase text-slate-500">
          Revision
          <input name="action_revision" defaultValue={params.get(context?.withinAction ? "revision" : "action_revision") ?? context?.actionRevision ?? ""} placeholder="Exact revision" />
        </label>
        <FilterLabel label="Execution"><input name="execution_id" defaultValue={params.get("execution_id") ?? context?.executionId ?? ""} placeholder="Exact execution ID" /></FilterLabel>
        <FilterLabel label="Attempt"><input name="attempt_id" defaultValue={params.get("attempt_id") ?? context?.attemptId ?? ""} placeholder="Exact attempt ID" /></FilterLabel>
        <FilterLabel label="Runtime Host"><input name="runtime_host_id" defaultValue={params.get("runtime_host_id") ?? context?.runtimeHostId ?? ""} placeholder="Exact host ID" /></FilterLabel>
        <FilterLabel label="Severity"><select name="severity" defaultValue={params.get("severity") ?? ""}><option value="">All severities</option>{["trace", "debug", "info", "warn", "error"].map((severity) => <option key={severity} value={severity}>{severity}</option>)}</select></FilterLabel>
        <FilterLabel label="Text"><input name="search" type="search" defaultValue={params.get("search") ?? ""} maxLength={256} placeholder="Message contains" /></FilterLabel>
        <FilterLabel label="Group"><select name="group" defaultValue={groupMode}><option value="runtime_host">Runtime Host</option><option value="execution">Execution</option><option value="time">Time</option></select></FilterLabel>
        <div className="flex gap-2 md:col-span-2 xl:col-span-4"><Button type="submit">Filter</Button><a className="inline-flex min-h-9 items-center justify-center rounded-md border border-white/10 px-3 text-sm font-semibold text-slate-400 hover:bg-white/[0.04] hover:text-white" href={logHref(cleared, context)}>Clear</a></div>
      </form>
    </Panel>
  );
}

function StreamCard({
  stream,
  selected,
  params,
  context,
  completeness,
}: {
  stream: LogStreamSummary;
  selected: boolean;
  params: URLSearchParams;
  context?: LogViewContext;
  completeness: LogCompleteness;
}) {
  const next = new URLSearchParams(params);
  next.set("runtime_host_id", stream.runtime_host_id);
  const loss = lossCount(stream);

  return (
    <a href={logHref(next, context)} className="rounded-lg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300">
      <Panel className={cn("grid gap-3 p-3 transition-colors hover:border-white/20", selected && "border-violet-400/40 bg-violet-500/[0.06]")}>
        <div className="flex min-w-0 items-start justify-between gap-3">
          <div className="min-w-0">
            <span className="block truncate font-mono text-xs font-semibold text-slate-100" title={stream.runtime_host_id}>{stream.runtime_host_id}</span>
            <span className="block truncate text-xs text-slate-500">{stream.action_key_id} · {stream.action_revision}</span>
          </div>
          <Badge tone={completenessTone(completeness)}>{completeness}</Badge>
        </div>
        <div className="grid grid-cols-3 gap-2 font-mono text-[11px] tabular-nums">
          <Stat label="Records" value={formatInteger(stream.persisted_record_count)} />
          <Stat label="Lost" value={formatInteger(loss)} tone={loss === "0" ? undefined : "text-amber-200"} />
          <Stat label="Range" value={sequenceSpan(stream)} />
        </div>
        {stream.loss_ranges.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {stream.loss_ranges.map((range) => (
              <Badge key={`${range.cause}-${range.first_sequence}-${range.last_sequence}`} tone={lossTone(range.cause)}>
                {lossLabel(range.cause)} {formatRange(range)}
              </Badge>
            ))}
          </div>
        )}
      </Panel>
    </a>
  );
}

function Stat({ label, value, tone }: { label: string; value: string; tone?: string }) {
  return <span className="grid gap-0.5"><span className="uppercase text-slate-600">{label}</span><strong className={cn("truncate font-semibold text-slate-300", tone)} title={value}>{value}</strong></span>;
}

function RecordPanel({
  stream,
  records,
  query,
  context,
}: {
  stream?: LogStreamSummary;
  records: LogRecord[];
  query: {
    data: unknown;
    error: unknown;
    isError: boolean;
    hasNextPage: boolean;
    isFetchingNextPage: boolean;
    fetchNextPage: () => Promise<unknown>;
  };
  context?: LogViewContext;
}) {
  if (!stream) return <EmptyState title="Select a Runtime Host" message="Choose one stream to inspect its sequence-ordered records." />;
  if (query.isError && !query.data) return <EmptyState title="Log records unavailable" message={errorMessage(query.error)} />;
  if (!query.data) return <EmptyState title="Loading records" message={stream.runtime_host_id} />;

  const entries = sequenceEntries(records, stream.loss_ranges);
  return (
    <Panel className="min-w-0 overflow-hidden">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-white/10 px-4 py-3">
        <div className="min-w-0">
          <h2 className="truncate font-mono text-sm font-semibold text-white">{stream.runtime_host_id}</h2>
          <p className="text-xs text-slate-500">Ascending stream sequence · {stream.runtime_language}</p>
        </div>
        <span className="font-mono text-[11px] text-slate-600">Started {formatInteger(stream.started_at_unix_nanos)} ns</span>
      </div>
      {query.isError && <p className="border-b border-white/10 px-4 py-2 text-sm text-red-300">{errorMessage(query.error)}</p>}
      {entries.length === 0 ? (
        <div className="p-6 text-center text-sm text-slate-500">No records were persisted for this stream.</div>
      ) : (
        <ol className="divide-y divide-white/[0.06]">
          {entries.map((entry) => entry.kind === "loss" ? (
            <LossMarker key={`loss-${entry.range.cause}-${entry.range.first_sequence}-${entry.range.last_sequence}`} range={entry.range} />
          ) : (
            <RecordRow key={`record-${entry.record.stream_sequence}`} record={entry.record} context={context} />
          ))}
        </ol>
      )}
      {query.hasNextPage && (
        <div className="border-t border-white/10 p-3">
          <Button type="button" onClick={() => void query.fetchNextPage()} disabled={query.isFetchingNextPage}>
            {query.isFetchingNextPage ? "Loading…" : "Load more records"}
          </Button>
        </div>
      )}
    </Panel>
  );
}

function RecordRow({ record, context }: { record: LogRecord; context?: LogViewContext }) {
  const attributes = Object.keys(record.attributes).length;
  return (
    <li className="grid grid-cols-[72px_12px_minmax(0,1fr)] gap-2 px-3 py-3 sm:grid-cols-[96px_12px_minmax(0,1fr)]">
      <code className="truncate pt-0.5 text-right text-[11px] tabular-nums text-slate-600" title={record.stream_sequence}>{formatInteger(record.stream_sequence)}</code>
      <span className={cn("mt-1.5 h-2 w-2 rounded-full", severityDot(record.severity))} />
      <div className="min-w-0 space-y-2">
        <div className="flex flex-wrap items-center gap-1.5">
          <Badge tone={record.correlation ? "violet" : "slate"}>{record.correlation ? "application" : "lifecycle"}</Badge>
          <Badge tone={severityTone(record.severity)}>{record.severity}</Badge>
          <span className="font-mono text-[11px] text-slate-600" title={`${record.timestamp_unix_nanos} ns`}>{formatUnixNanos(record.timestamp_unix_nanos)}</span>
        </div>
        <p className="whitespace-pre-wrap break-words font-mono text-xs leading-5 text-slate-200">{record.message || "(empty message)"}</p>
        <div className="flex flex-wrap gap-x-3 gap-y-1 font-mono text-[11px] text-slate-500">
          {record.runtime_session_id && <span>session {record.runtime_session_id}</span>}
          {record.correlation && (
            <>
              <a className="text-cyan-300 hover:text-cyan-100" href={executionHref(record, context)}>execution {record.correlation.execution_id}</a>
              <a className="text-violet-300 hover:text-violet-100" href={attemptHref(record, context)}>attempt {record.correlation.attempt_number} · {record.correlation.attempt_id}</a>
            </>
          )}
          <span>host {record.runtime_host_id}</span>
          {record.trace_id && <span>trace {record.trace_id}{record.span_id ? ` / ${record.span_id}` : ""}</span>}
        </div>
        {attributes > 0 && (
          <details className="font-mono text-[11px] text-slate-500">
            <summary className="w-fit cursor-pointer rounded-sm hover:text-slate-300 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-300">{attributes} attribute{attributes === 1 ? "" : "s"}</summary>
            <pre className="mt-2 overflow-auto rounded-md border border-white/10 bg-[#08090b] p-2 text-slate-300">{JSON.stringify(record.attributes, null, 2)}</pre>
          </details>
        )}
      </div>
    </li>
  );
}

function LossMarker({ range }: { range: LogLossRange }) {
  return (
    <li className="grid grid-cols-[72px_12px_minmax(0,1fr)] gap-2 bg-amber-400/[0.035] px-3 py-2 sm:grid-cols-[96px_12px_minmax(0,1fr)]">
      <code className="truncate text-right text-[11px] tabular-nums text-amber-300/70" title={formatRange(range)}>{formatRange(range)}</code>
      <span className="mt-1 h-2 w-2 rotate-45 border border-amber-300/60 bg-amber-400/10" />
      <span className="font-mono text-[11px] text-amber-200">Missing sequence range · {lossLabel(range.cause)}</span>
    </li>
  );
}

function sequenceEntries(records: LogRecord[], ranges: LogLossRange[]) {
  const entries: Array<{ kind: "record"; sequence: bigint; record: LogRecord } | { kind: "loss"; sequence: bigint; range: LogLossRange }> = [
    ...records.map((record) => ({ kind: "record" as const, sequence: BigInt(record.stream_sequence), record })),
    ...ranges.map((range) => ({ kind: "loss" as const, sequence: BigInt(range.first_sequence), range })),
  ];
  return entries.sort((left, right) => left.sequence < right.sequence ? -1 : left.sequence > right.sequence ? 1 : left.kind === "loss" ? -1 : 1);
}

export function projectLogCompleteness(
  stream: LogStreamSummary,
  execution: ExecutionAggregate | undefined,
  executionId?: string,
  attemptId?: string,
  activeRuntimeHosts?: ReadonlySet<string>,
): LogCompleteness {
  if (stream.completeness !== "active" || !executionId) return stream.completeness;
  const authoritativeAttemptId = activeAttemptId(execution);
  if (!authoritativeAttemptId) return "unknown";
  if (attemptId) return authoritativeAttemptId === attemptId ? "active" : "unknown";
  return activeRuntimeHosts?.has(stream.runtime_host_id) ? "active" : "unknown";
}

function activeAttemptId(execution?: ExecutionAggregate) {
  if (!execution || !["running", "cancellation_requested"].includes(execution.state)) return undefined;
  const active = execution.attempts.find((item) => item.attempt.attempt_id === execution.active_attempt_id);
  return active && ["running", "cancellation_requested"].includes(active.state) ? active.attempt.attempt_id : undefined;
}

async function loadAttemptRuntimeHosts(executionId: string, attemptId: string) {
  const hosts = new Set<string>();
  let cursor: string | undefined;
  do {
    const page = await logsApi.streams({ execution_id: executionId, attempt_id: attemptId }, cursor, 1000);
    page.streams.forEach((stream) => hosts.add(stream.runtime_host_id));
    cursor = page.next_cursor ?? undefined;
  } while (cursor);
  return hosts;
}

function logFilters(params: URLSearchParams, context?: LogViewContext): LogFilters {
  const filters: LogFilters = context?.actionId ? { action_key_id: context.actionId } : {};
  for (const key of ["action_key_id", "runtime_host_id", "execution_id", "attempt_id", "severity", "search"] as const) {
    if (key === "action_key_id" && context?.withinAction) continue;
    const value = params.get(key);
    if (value) filters[key] = value;
  }
  const revision = params.get(context?.withinAction ? "revision" : "action_revision") ?? context?.actionRevision;
  if (revision) filters.action_revision = revision;
  if (context?.executionId && !filters.execution_id) filters.execution_id = context.executionId;
  if (context?.attemptId && !filters.attempt_id) filters.attempt_id = context.attemptId;
  if (context?.runtimeHostId && !filters.runtime_host_id) filters.runtime_host_id = context.runtimeHostId;
  return filters;
}

function recordFilters(filters: LogFilters): LogRecordFilters {
  const { execution_id, attempt_id, severity, search } = filters;
  return { execution_id, attempt_id, severity, search };
}

function logGroupMode(params: URLSearchParams): LogGroupMode {
  const value = params.get("group");
  return value === "execution" || value === "time" ? value : "runtime_host";
}

function baseLogParams(context?: LogViewContext) {
  return context?.withinAction && context.actionId
    ? new URLSearchParams({ action_id: context.actionId, tab: "logs" })
    : new URLSearchParams();
}

function logHref(params: URLSearchParams, context?: LogViewContext) {
  const next = new URLSearchParams(params);
  if (context?.withinAction && context.actionId) {
    next.set("action_id", context.actionId);
    next.set("tab", "logs");
    next.delete("action_key_id");
    return `#actions?${next}`;
  }
  return next.size ? `#logs?${next}` : "#logs";
}

function navigateLogs(params: URLSearchParams, context?: LogViewContext) {
  window.location.hash = logHref(params, context).slice(1);
}

function executionHref(record: LogRecord, context?: LogViewContext) {
  if (context?.withinAction && context.actionId && record.correlation) {
    const query = new URLSearchParams({
      action_id: context.actionId,
      tab: "executions",
      revision: record.action_revision,
      execution_id: record.correlation.execution_id,
    });
    return `#actions?${query}`;
  }
  return `#execution-preview?id=${encodeURIComponent(record.correlation?.execution_id ?? "")}`;
}

function attemptHref(record: LogRecord, context?: LogViewContext) {
  const query = baseLogParams(context);
  if (context?.withinAction) query.set("revision", record.action_revision);
  query.set("execution_id", record.correlation?.execution_id ?? "");
  query.set("attempt_id", record.correlation?.attempt_id ?? "");
  query.set("runtime_host_id", record.runtime_host_id);
  return logHref(query, context);
}

function FilterLabel({ label, children }: { label: string; children: ReactNode }) {
  return <label className="grid gap-1 font-mono text-[11px] font-bold uppercase text-slate-500">{label}{children}</label>;
}

function formatUnixNanos(value: string) {
  try {
    const date = new Date(Number(BigInt(value) / 1_000_000n));
    return Number.isNaN(date.getTime()) ? `${formatInteger(value)} ns` : date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" });
  } catch {
    return `${value} ns`;
  }
}

function lossCount(stream: LogStreamSummary) {
  try {
    return (BigInt(stream.ingestion_dropped_count) + BigInt(stream.provider_dropped_count) + BigInt(stream.evicted_record_count)).toString();
  } catch {
    return "?";
  }
}

function sequenceSpan(stream: LogStreamSummary) {
  if (!stream.first_sequence || !stream.last_sequence) return "—";
  return stream.first_sequence === stream.last_sequence ? formatInteger(stream.first_sequence) : `${formatInteger(stream.first_sequence)}–${formatInteger(stream.last_sequence)}`;
}

function formatRange(range: LogLossRange) {
  return range.first_sequence === range.last_sequence ? formatInteger(range.first_sequence) : `${formatInteger(range.first_sequence)}–${formatInteger(range.last_sequence)}`;
}

function formatInteger(value: string) {
  try {
    return BigInt(value).toLocaleString("en-US");
  } catch {
    return value;
  }
}

function lossLabel(cause: LogLossCause) {
  return cause === "ingestion_overflow" ? "ingestion overflow" : cause === "provider_failure" ? "provider failure" : "retention eviction";
}

function lossTone(cause: LogLossCause): "amber" | "red" | "slate" {
  return cause === "ingestion_overflow" ? "amber" : cause === "provider_failure" ? "red" : "slate";
}

function completenessTone(value: LogCompleteness): "blue" | "green" | "amber" | "slate" {
  return value === "active" ? "blue" : value === "complete" ? "green" : value === "incomplete" ? "amber" : "slate";
}

function severityTone(value: string): "red" | "amber" | "blue" | "slate" {
  const severity = value.toLowerCase();
  return ["error", "fatal"].includes(severity) ? "red" : severity === "warn" ? "amber" : severity === "info" ? "blue" : "slate";
}

function severityDot(value: string) {
  const severity = value.toLowerCase();
  return ["error", "fatal"].includes(severity) ? "bg-red-400" : severity === "warn" ? "bg-amber-300" : severity === "info" ? "bg-blue-400" : "bg-slate-500";
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : "Request failed";
}
