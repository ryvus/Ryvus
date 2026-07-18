import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { logsApi, type LogFilters, type LogRecord, type LogStreamSummary } from "../api/logs";
import { Badge, Button, EmptyState, Panel, cn } from "../components/ui";
import type { LogViewContext } from "./Logs";

type ProjectionFilters = {
  execution_id?: string;
  attempt_id?: string;
  runtime_host_id?: string;
  severity?: string;
  search?: string;
};

export function ActionRevisionLogs({ context }: { context: LogViewContext }) {
  const [hash, setHash] = useState(window.location.hash);
  const [records, setRecords] = useState<LogRecord[]>([]);
  const [olderCursor, setOlderCursor] = useState<string>();
  const [newerCursor, setNewerCursor] = useState<string>();
  const [hasOlder, setHasOlder] = useState(false);
  const [paging, setPaging] = useState<"older" | "newer">();
  const [pageError, setPageError] = useState<string>();
  const consoleRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const update = () => setHash(window.location.hash);
    window.addEventListener("hashchange", update);
    return () => window.removeEventListener("hashchange", update);
  }, []);

  const params = new URLSearchParams(hash.split("?")[1] ?? "");
  const actionId = context.actionId ?? "";
  const revision = params.get("revision") ?? context.actionRevision ?? "";
  const filters = projectionFilters(params, context);
  const filterKey = JSON.stringify([actionId, revision, filters]);
  const initial = useQuery({
    queryKey: ["action-revision-log-records", actionId, revision, filters],
    queryFn: () => logsApi.projectedRecords(actionId, revision, filters),
    enabled: Boolean(actionId && revision),
  });
  const streams = useInfiniteQuery({
    queryKey: ["action-revision-log-streams", actionId, revision, filters],
    queryFn: ({ pageParam }) => logsApi.streams({
      action_key_id: actionId,
      action_revision: revision,
      ...filters,
    }, pageParam, 1000),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: Boolean(actionId && revision),
  });

  useEffect(() => {
    setRecords(initial.data?.records ?? []);
    setOlderCursor(initial.data?.older_cursor ?? undefined);
    setNewerCursor(initial.data?.newer_cursor ?? undefined);
    setHasOlder(initial.data?.has_older ?? false);
    setPageError(undefined);
  }, [filterKey, initial.data]);

  async function load(direction: "older" | "newer") {
    const cursor = direction === "older" ? olderCursor : newerCursor;
    if (!cursor || paging) return;
    setPaging(direction);
    setPageError(undefined);
    const viewport = consoleRef.current;
    const previousHeight = viewport?.scrollHeight ?? 0;
    const previousTop = viewport?.scrollTop ?? 0;
    try {
      const page = await logsApi.projectedRecords(actionId, revision, filters, { direction, value: cursor });
      setRecords((current) => direction === "older" ? prepend(current, page.records) : append(current, page.records));
      setOlderCursor((current) => direction === "older" ? page.older_cursor ?? current : current);
      setNewerCursor((current) => direction === "newer" ? page.newer_cursor ?? current : current);
      setHasOlder(direction === "older" ? page.has_older : hasOlder);
      if (direction === "older" && viewport) {
        requestAnimationFrame(() => {
          viewport.scrollTop = previousTop + viewport.scrollHeight - previousHeight;
        });
      }
    } catch (error) {
      setPageError(errorMessage(error));
    } finally {
      setPaging(undefined);
    }
  }

  if (!revision) return <EmptyState title="Revision required" message="Select an exact Action revision to inspect logs." />;

  const streamItems = streams.data?.pages.flatMap((page) => page.streams) ?? [];
  const summary = summarizeStreams(streamItems);

  return (
    <div className="grid gap-4">
      <FilterForm params={params} context={context} revision={revision} />
      <Panel className="overflow-hidden">
        <div className="grid gap-3 border-b border-white/10 px-4 py-3 lg:grid-cols-[minmax(0,1fr)_auto] lg:items-center">
          <div>
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="font-mono text-sm font-semibold text-white">Revision {revision}</h2>
              <Badge tone="violet">{summary.hosts} Runtime Host{summary.hosts === 1 ? "" : "s"}</Badge>
              {summary.states.map((state) => <Badge key={state} tone={state === "incomplete" ? "amber" : state === "complete" ? "green" : state === "active" ? "blue" : "slate"}>{state}</Badge>)}
            </div>
            <p className="mt-1 text-xs text-slate-500">Records from independent Runtime Hosts are ordered observationally by observed time. Stream sequence remains authoritative only within each host.</p>
          </div>
          <div className="flex flex-wrap items-center gap-2 font-mono text-[11px] text-slate-500">
            <span>{summary.records} committed</span>
            <span className={cn(summary.lost > 0n && "text-amber-200")}>{summary.lost.toLocaleString()} lost</span>
            {streams.hasNextPage && <Button type="button" onClick={() => void streams.fetchNextPage()} disabled={streams.isFetchingNextPage}>Load stream summary</Button>}
          </div>
        </div>
        {streamItems.some((stream) => stream.loss_ranges.length > 0) && <LossSummary streams={streamItems} />}
        {hasOlder && (
          <div className="border-b border-white/10 px-3 py-2 text-center">
            <Button type="button" onClick={() => void load("older")} disabled={Boolean(paging)}>{paging === "older" ? "Loading…" : "Load older"}</Button>
          </div>
        )}
        {initial.isError && !initial.data ? (
          <EmptyState title="Log history unavailable" message={errorMessage(initial.error)} />
        ) : !initial.data ? (
          <EmptyState title="Loading logs" message={`Reading revision ${revision}.`} />
        ) : records.length === 0 ? (
          <EmptyState title="No logs" message="No committed records match these filters." />
        ) : (
          <div ref={consoleRef} className="max-h-[68vh] overflow-auto">
            <ol className="py-1">
              {records.map((record) => <RecordRow key={recordKey(record)} record={record} params={params} context={context} />)}
            </ol>
          </div>
        )}
        {(pageError || records.length > 0) && (
          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-white/10 px-3 py-2">
            <span className="text-xs text-red-300">{pageError}</span>
            <Button type="button" onClick={() => void load("newer")} disabled={Boolean(paging || !newerCursor)}>{paging === "newer" ? "Checking…" : "Refresh newer"}</Button>
          </div>
        )}
      </Panel>
    </div>
  );
}

function FilterForm({ params, context, revision }: { params: URLSearchParams; context: LogViewContext; revision: string }) {
  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const next = new URLSearchParams(params);
    const before = projectionFilters(params, context);
    for (const key of ["revision", "execution_id", "attempt_id", "runtime_host_id", "severity", "search"] as const) {
      const value = String(form.get(key) ?? "").trim();
      value ? next.set(key, value) : next.delete(key);
    }
    if (next.get("revision") !== revision) {
      next.delete("execution_id"); next.delete("attempt_id"); next.delete("runtime_host_id");
    } else if (next.get("execution_id") !== (before.execution_id ?? null)) {
      next.delete("attempt_id"); next.delete("runtime_host_id");
    } else if (next.get("attempt_id") !== (before.attempt_id ?? null)) {
      next.delete("runtime_host_id");
    }
    navigate(next, context);
  }
  const clear = new URLSearchParams({ action_id: context.actionId ?? "", tab: "logs", revision });
  return (
    <Panel className="p-3">
      <form key={params.toString()} className="grid gap-3 md:grid-cols-2 xl:grid-cols-3" onSubmit={submit}>
        <FilterLabel label="Revision"><input name="revision" defaultValue={revision} placeholder="Exact revision" /></FilterLabel>
        <FilterLabel label="Execution"><input name="execution_id" defaultValue={params.get("execution_id") ?? ""} placeholder="Exact execution ID" /></FilterLabel>
        <FilterLabel label="Attempt"><input name="attempt_id" defaultValue={params.get("attempt_id") ?? ""} placeholder="Exact attempt ID" /></FilterLabel>
        <FilterLabel label="Runtime Host"><input name="runtime_host_id" defaultValue={params.get("runtime_host_id") ?? ""} placeholder="Exact host ID" /></FilterLabel>
        <FilterLabel label="Severity"><select name="severity" defaultValue={params.get("severity") ?? ""}><option value="">All severities</option>{["trace", "debug", "info", "warn", "error"].map((value) => <option key={value}>{value}</option>)}</select></FilterLabel>
        <FilterLabel label="Text"><input name="search" type="search" maxLength={256} defaultValue={params.get("search") ?? ""} placeholder="Message contains" /></FilterLabel>
        <div className="flex gap-2 md:col-span-2 xl:col-span-3"><Button type="submit">Filter</Button><a className="inline-flex min-h-9 items-center rounded-md border border-white/10 px-3 text-sm font-semibold text-slate-400 hover:text-white" href={`#actions?${clear}`}>Clear</a></div>
      </form>
    </Panel>
  );
}

function RecordRow({ record, params, context }: { record: LogRecord; params: URLSearchParams; context: LogViewContext }) {
  return (
    <li className="grid grid-cols-[96px_32px_minmax(0,1fr)] gap-x-2 px-3 py-0.5 font-mono text-xs leading-5">
      <time className="tabular-nums text-slate-600" title={`${record.observed_timestamp_unix_nanos} ns`}>{formatLogTime(record.observed_timestamp_unix_nanos)}</time>
      <span className={cn("font-semibold", severityText(record.severity))}>{compactLevel(record.severity)}</span>
      <span className="min-w-0 whitespace-pre-wrap break-words text-slate-200">
        {record.message || "(empty message)"}
        {record.correlation && (
          <span className="ml-3 inline-flex flex-wrap gap-x-2 text-[10px] text-slate-600">
            <FilterLink label={`execution=${record.correlation.execution_id}`} name="execution_id" value={record.correlation.execution_id} params={params} context={context} />
            <FilterLink label={`attempt=${record.correlation.attempt_number}`} name="attempt_id" value={record.correlation.attempt_id} params={params} context={context} executionId={record.correlation.execution_id} />
          </span>
        )}
      </span>
    </li>
  );
}

function FilterLink({ label, name, value, params, context, executionId }: { label: string; name: "execution_id" | "attempt_id" | "runtime_host_id"; value: string; params: URLSearchParams; context: LogViewContext; executionId?: string }) {
  const next = new URLSearchParams(params);
  if (executionId) next.set("execution_id", executionId);
  next.set(name, value);
  if (name === "execution_id") { next.delete("attempt_id"); next.delete("runtime_host_id"); }
  if (name === "attempt_id") next.delete("runtime_host_id");
  next.set("action_id", context.actionId ?? ""); next.set("tab", "logs");
  return <a className="text-cyan-300 hover:text-cyan-100" href={`#actions?${next}`}>{label}</a>;
}

function LossSummary({ streams }: { streams: LogStreamSummary[] }) {
  return <details className="border-b border-amber-300/10 bg-amber-300/[0.03] px-4 py-2 text-xs text-amber-100"><summary className="cursor-pointer">Committed loss by Runtime Host</summary><ul className="mt-2 grid gap-1 font-mono text-[11px] text-amber-200/80">{streams.flatMap((stream) => stream.loss_ranges.map((range) => <li key={`${stream.runtime_host_id}-${range.cause}-${range.first_sequence}`}>{stream.runtime_host_id} · {range.cause} · {range.first_sequence}–{range.last_sequence}</li>))}</ul></details>;
}

function summarizeStreams(streams: LogStreamSummary[]) {
  const states = [...new Set(streams.map((stream) => stream.completeness))].sort();
  let records = 0n; let lost = 0n;
  for (const stream of streams) {
    records += toBigInt(stream.persisted_record_count);
    lost += toBigInt(stream.ingestion_dropped_count) + toBigInt(stream.provider_dropped_count) + toBigInt(stream.evicted_record_count);
  }
  return { hosts: streams.length, states, records: records.toLocaleString(), lost };
}

function projectionFilters(params: URLSearchParams, context: LogViewContext): ProjectionFilters {
  const filters: ProjectionFilters = {};
  for (const key of ["execution_id", "attempt_id", "runtime_host_id", "severity", "search"] as const) {
    const value = params.get(key) ?? (key === "execution_id" ? context.executionId : key === "attempt_id" ? context.attemptId : key === "runtime_host_id" ? context.runtimeHostId : undefined);
    if (value) filters[key] = value;
  }
  return filters;
}

function navigate(params: URLSearchParams, context: LogViewContext) { params.set("action_id", context.actionId ?? ""); params.set("tab", "logs"); window.location.hash = `actions?${params}`; }
function recordKey(record: LogRecord) { return `${record.runtime_host_id}:${record.stream_sequence}`; }
function prepend(current: LogRecord[], older: LogRecord[]) { const keys = new Set(current.map(recordKey)); return [...older.filter((record) => !keys.has(recordKey(record))), ...current]; }
function append(current: LogRecord[], newer: LogRecord[]) { const keys = new Set(current.map(recordKey)); return [...current, ...newer.filter((record) => !keys.has(recordKey(record)))]; }
function toBigInt(value: string) { try { return BigInt(value); } catch { return 0n; } }
function compactLevel(value: string) { return ({ trace: "TRC", debug: "DBG", info: "INF", warn: "WRN", error: "ERR" }[value.toLowerCase()] ?? value.toUpperCase().slice(0, 3).padEnd(3)); }
function severityText(value: string) { const level = value.toLowerCase(); return level === "error" ? "text-red-300" : level === "warn" ? "text-amber-300" : level === "info" ? "text-blue-300" : "text-slate-500"; }
function formatLogTime(value: string) { try { const date = new Date(Number(BigInt(value) / 1_000_000n)); return Number.isNaN(date.getTime()) ? value : date.toLocaleTimeString(undefined, { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit", fractionalSecondDigits: 3 }); } catch { return value; } }
function errorMessage(error: unknown) { return error instanceof Error ? error.message : "Request failed"; }
function FilterLabel({ label, children }: { label: string; children: ReactNode }) { return <label className="grid gap-1 font-mono text-[11px] font-bold uppercase text-slate-500">{label}{children}</label>; }
