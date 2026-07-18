import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { historyApi, type ExecutionAggregate, type ExecutionFilters } from "../api/history";
import { logsApi } from "../api/logs";
import { Badge, Button, EmptyState, Page, Panel } from "../components/ui";

export type ExecutionViewContext = {
  actionId?: string;
  actionRevision?: string;
  executionId?: string;
  withinAction?: boolean;
};

export function Executions({ context }: { context?: ExecutionViewContext }) {
  const [hash, setHash] = useState(window.location.hash);
  useEffect(() => {
    const update = () => setHash(window.location.hash);
    window.addEventListener("hashchange", update);
    return () => window.removeEventListener("hashchange", update);
  }, []);

  const params = new URLSearchParams(hash.split("?")[1] ?? "");
  const withinAction = Boolean(context?.withinAction && context.actionId);
  const id = context?.executionId ?? params.get(withinAction ? "execution_id" : "id") ?? undefined;
  const filters = executionFilters(params, context);
  const execution = useQuery({
    queryKey: ["execution", id],
    queryFn: () => historyApi.execution(id!),
    enabled: Boolean(id),
  });
  const executions = useInfiniteQuery({
    queryKey: ["executions", filters],
    queryFn: ({ pageParam }) => historyApi.executions(filters, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: !id,
  });

  let content: ReactNode;
  if (id) {
    content = execution.isError && !execution.data
      ? <EmptyState title="Execution unavailable" message={errorMessage(execution.error)} />
      : !execution.data
        ? <EmptyState title="Loading execution" message={id} />
        : withinAction && execution.data.action_id !== context?.actionId
          ? <EmptyState title="Execution not found for this action" message="The selected execution does not belong to this action." />
          : <ExecutionDetail item={execution.data} params={params} context={context} />;
  } else if (!executions.data) {
    content = <div className="grid gap-4"><ExecutionFiltersForm params={params} context={context} />{executions.isError ? <p role="alert" className="text-sm text-red-300">{errorMessage(executions.error)}</p> : <EmptyState title="Loading executions" message="Reading durable execution state." />}</div>;
  } else {
    const items = executions.data.pages.flatMap((page) => page.items);
    content = (
      <div className="grid gap-4">
        <ExecutionFiltersForm params={params} context={context} />
        {executions.error && <p className="text-sm text-red-300">{errorMessage(executions.error)}</p>}
        {items.length ? (
          <Panel className="overflow-x-auto">
            <table className="w-full min-w-[1040px] border-collapse text-left text-sm">
              <thead className="border-b border-white/10 font-mono text-[11px] uppercase text-slate-500">
                <tr><th className="px-4 py-3">Execution</th><th className="px-4 py-3">Started</th><th className="px-4 py-3">Latest attempt duration</th><th className="px-4 py-3">Status</th><th className="px-4 py-3">Attempts</th><th className="px-4 py-3">Runtime Hosts</th><th className="px-4 py-3">Trigger</th></tr>
              </thead>
              <tbody className="divide-y divide-white/[0.07]">
                {items.map((item) => <ExecutionRow key={item.execution_id} item={item} params={params} context={context} />)}
              </tbody>
            </table>
          </Panel>
        ) : <EmptyState title="No executions" message="No matching durable executions were found." />}
        {executions.hasNextPage && <Button type="button" className="justify-self-start" onClick={() => void executions.fetchNextPage()} disabled={executions.isFetchingNextPage}>{executions.isFetchingNextPage ? "Loading…" : "Load more"}</Button>}
      </div>
    );
  }

  if (withinAction) return content;
  return <Page eyebrow="Execution History" title={id ? id : filters.action_id ? `Executions for ${filters.action_id}` : "Executions"}>{content}</Page>;
}

function ExecutionFiltersForm({ params, context }: { params: URLSearchParams; context?: ExecutionViewContext }) {
  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const afterInput = event.currentTarget.elements.namedItem("created_after") as HTMLInputElement;
    const beforeInput = event.currentTarget.elements.namedItem("created_before") as HTMLInputElement;
    if (Number.isFinite(afterInput.valueAsNumber) && Number.isFinite(beforeInput.valueAsNumber) && afterInput.valueAsNumber >= beforeInput.valueAsNumber) {
      afterInput.setCustomValidity("Created after must be earlier than created before.");
      afterInput.reportValidity();
      return;
    }
    const form = new FormData(event.currentTarget);
    const next = new URLSearchParams(params);
    for (const name of ["state", "trigger", "search"] as const) setParam(next, name, form.get(name));
    setDateParam(next, "created_after_unix_ms", form.get("created_after"));
    setDateParam(next, "created_before_unix_ms", form.get("created_before"));
    if (context?.withinAction) setParam(next, "revision", form.get("revision"));
    else setParam(next, "action_revision", form.get("revision"));
    next.delete("id");
    next.delete("execution_id");
    next.delete("cursor");
    navigate(next, context);
  }

  const cleared = new URLSearchParams();
  if (context?.withinAction && context.actionId) {
    cleared.set("action_id", context.actionId);
    cleared.set("tab", "executions");
  }
  const revision = params.get(context?.withinAction ? "revision" : "action_revision") ?? context?.actionRevision ?? "";
  return (
    <Panel className="p-3">
      <form key={params.toString()} className="grid gap-3 md:grid-cols-2 xl:grid-cols-[minmax(150px,0.8fr)_minmax(180px,1fr)_minmax(130px,0.7fr)_minmax(190px,1fr)_minmax(190px,1fr)_auto_auto] xl:items-end" onSubmit={submit}>
        <FilterLabel label="Status"><select name="state" defaultValue={params.get("state") ?? ""} className={controlClass}><option value="">All statuses</option>{["pending", "running", "cancellation_requested", "succeeded", "failed", "cancelled", "timed_out"].map((value) => <option key={value} value={value}>{labelState(value)}</option>)}</select></FilterLabel>
        <FilterLabel label="Exact revision"><input name="revision" defaultValue={revision} placeholder="Revision" /></FilterLabel>
        <FilterLabel label="Trigger"><select name="trigger" defaultValue={params.get("trigger") ?? ""} className={controlClass}><option value="">All triggers</option>{["api", "schedule", "flow", "manual", "queue", "unknown"].map((value) => <option key={value} value={value}>{labelState(value)}</option>)}</select></FilterLabel>
        <FilterLabel label="Created after"><input name="created_after" type="datetime-local" defaultValue={dateInputValue(params.get("created_after_unix_ms"))} onInput={(event) => event.currentTarget.setCustomValidity("")} /></FilterLabel>
        <FilterLabel label="Created before"><input name="created_before" type="datetime-local" defaultValue={dateInputValue(params.get("created_before_unix_ms"))} onInput={(event) => (event.currentTarget.form?.elements.namedItem("created_after") as HTMLInputElement | null)?.setCustomValidity("")} /></FilterLabel>
        <FilterLabel label="Execution ID"><input name="search" type="search" defaultValue={params.get("search") ?? ""} placeholder="Exact or prefix" /></FilterLabel>
        <div className="flex gap-2 md:col-span-2 xl:col-span-1"><Button type="submit">Filter</Button><a className="inline-flex min-h-9 items-center justify-center rounded-md border border-white/10 px-3 text-sm font-semibold text-slate-400 hover:bg-white/[0.04] hover:text-white" href={hashFor(cleared, context)}>Clear</a></div>
      </form>
    </Panel>
  );
}

function ExecutionRow({ item, params, context }: { item: ExecutionAggregate; params: URLSearchParams; context?: ExecutionViewContext }) {
  return (
    <tr className="transition-colors hover:bg-white/[0.035]">
      <td className="p-0"><a href={executionHref(item, params, context)} className="block min-h-11 max-w-80 px-4 py-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-violet-300"><code className="block truncate text-xs text-cyan-200" title={item.execution_id}>{item.execution_id}</code><span className="mt-1 block truncate text-xs text-slate-600" title={context?.withinAction ? item.action_revision : `${item.action_id} · ${item.action_revision}`}>{context?.withinAction ? item.action_revision : `${item.action_id} · ${item.action_revision}`}</span></a></td>
      <td className="px-4 py-3 text-slate-400">{formatTimestamp(executionStartedAt(item))}</td>
      <td className="px-4 py-3 font-mono text-xs tabular-nums text-slate-400">{latestDuration(item)}</td>
      <td className="px-4 py-3"><Badge tone={stateTone(item.state)}>{item.state}</Badge></td>
      <td className="px-4 py-3 font-mono tabular-nums text-slate-400">{item.attempts.length}</td>
      <td className="px-4 py-3"><RuntimeHosts execution={item} context={context} /></td>
      <td className="px-4 py-3"><Badge tone={triggerTone(item.trigger.type)}>{item.trigger.type}</Badge></td>
    </tr>
  );
}

function ExecutionDetail({ item, params, context }: { item: ExecutionAggregate; params: URLSearchParams; context?: ExecutionViewContext }) {
  const backParams = new URLSearchParams(params);
  backParams.delete("id");
  backParams.delete("execution_id");
  const back = hashFor(backParams, context);
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <a href={back} className="rounded-sm font-mono text-xs text-cyan-300 hover:text-cyan-100 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300">← Executions</a>
        <LogLink execution={item} context={context}>View execution logs</LogLink>
      </div>
      <Panel className="grid gap-5 p-4 sm:p-5">
        <div className="flex flex-wrap items-start justify-between gap-3"><div className="min-w-0"><p className="font-mono text-[11px] font-bold uppercase text-slate-500">Execution</p><h2 className="mt-1 break-all font-mono text-sm text-white">{item.execution_id}</h2></div><Badge tone={stateTone(item.state)}>{item.state}</Badge></div>
        <dl className="grid gap-x-6 gap-y-4 sm:grid-cols-2 lg:grid-cols-4">
          <Meta label="Trigger" value={item.trigger.type} />
          <Meta label="Revision" value={item.action_revision} mono />
          <Meta label="Started" value={formatTimestamp(executionStartedAt(item))} />
          <Meta label="Completed" value={formatTimestamp(executionFinishedAt(item))} />
          <Meta label="Latest attempt duration" value={latestDuration(item)} mono />
          <Meta label="Cancellation intent" value={item.cancellation_intent ? `Requested · ${formatTimestamp(item.cancellation_intent.requested_at)}` : "Not requested"} />
          <Meta label="Timed out" value={timedOut(item) ? "Yes" : "No"} />
          <Meta label="Terminal outcome" value={item.terminal_state?.state ?? "Not terminal"} />
        </dl>
        <ScheduleLineage item={item} />
        <div><p className="font-mono text-[10px] font-bold uppercase text-slate-600">Observed Runtime Hosts</p><div className="mt-2"><RuntimeHosts execution={item} context={context} /></div></div>
      </Panel>
      <section className="grid gap-3" aria-labelledby="attempts-heading">
        <div><h2 id="attempts-heading" className="text-base font-semibold text-white">Attempts</h2><p className="mt-1 text-xs text-slate-500">Execution state is authoritative; Runtime Hosts are correlated observations.</p></div>
        {item.attempts.length ? item.attempts.map((attempt) => (
          <Panel key={attempt.attempt.attempt_id} className="grid gap-4 p-4">
            <div className="flex flex-wrap items-start justify-between gap-3"><div><p className="font-mono text-[11px] font-bold uppercase text-slate-500">Attempt {attempt.attempt.attempt_number}</p><code className="mt-1 block break-all text-xs text-slate-200">{attempt.attempt.attempt_id}</code></div><div className="flex flex-wrap gap-2"><Badge tone={stateTone(attempt.state)}>{attempt.state}</Badge>{attempt.outcome && <Badge tone={stateTone(attempt.outcome)}>{attempt.outcome}</Badge>}</div></div>
            <dl className="grid gap-x-6 gap-y-4 sm:grid-cols-2 lg:grid-cols-4"><Meta label="Started" value={formatTimestamp(attempt.started_at)} /><Meta label="Finished" value={formatTimestamp(attempt.finished_at)} /><Meta label="Duration" value={attempt.result ? formatDuration(attempt.result.duration) : "Unavailable"} mono /><Meta label="Result" value={attempt.outcome ?? attempt.state} /></dl>
            <div><p className="font-mono text-[10px] font-bold uppercase text-slate-600">Observed Runtime Hosts</p><div className="mt-2"><RuntimeHosts execution={item} attemptId={attempt.attempt.attempt_id} context={context} /></div></div>
            {attempt.result?.invocation_result.error && <FailureSummary error={attempt.result.invocation_result.error} />}
            <LogLink execution={item} attemptId={attempt.attempt.attempt_id} context={context}>View attempt logs</LogLink>
          </Panel>
        )) : <EmptyState title="No attempts" message="This execution has no recorded attempts." />}
      </section>
    </div>
  );
}

function RuntimeHosts({ execution, attemptId, context }: { execution: ExecutionAggregate; attemptId?: string; context?: ExecutionViewContext }) {
  // ponytail: one filtered stream query per execution; add batch correlation only if Portal history latency requires it.
  const hosts = useQuery({
    queryKey: ["execution-runtime-hosts", execution.execution_id, attemptId],
    queryFn: () => loadRuntimeHosts(execution.execution_id, attemptId),
  });
  if (hosts.isError) return <span className="text-xs text-amber-200">Unavailable</span>;
  if (!hosts.data) return <span className="text-xs text-slate-600">Loading…</span>;
  if (hosts.data.length === 0) return <span className="text-xs text-slate-600">None observed</span>;
  return <span className="flex flex-wrap gap-1.5">{hosts.data.map((host) => <a key={host} href={runtimeHostHref(execution, host, attemptId, context)} className="max-w-52 truncate rounded-sm font-mono text-xs text-cyan-300 hover:text-cyan-100 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300" title={host}>{host}</a>)}</span>;
}

async function loadRuntimeHosts(executionId: string, attemptId?: string) {
  const hosts = new Set<string>();
  let cursor: string | undefined;
  do {
    const page = await logsApi.streams({ execution_id: executionId, attempt_id: attemptId }, cursor, 1000);
    page.streams.forEach((stream) => hosts.add(stream.runtime_host_id));
    cursor = page.next_cursor ?? undefined;
  } while (cursor);
  return [...hosts].sort();
}

function LogLink({ execution, attemptId, context, children }: { execution: ExecutionAggregate; attemptId?: string; context?: ExecutionViewContext; children: string }) {
  return <a href={logsHref(execution, attemptId, context)} className="inline-flex min-h-9 w-fit items-center rounded-md border border-white/10 px-3 font-mono text-xs font-bold text-violet-200 hover:border-violet-300/30 hover:bg-violet-500/10 hover:text-white focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300">{children}</a>;
}

function FailureSummary({ error }: { error: { code: string; message: string; retryable: boolean; details?: unknown } }) {
  return <div className="rounded-md border border-red-400/20 bg-red-500/[0.05] p-3"><div className="flex flex-wrap items-center gap-2"><Badge tone="red">{error.code}</Badge>{error.retryable && <Badge tone="amber">Retryable</Badge>}</div><p className="mt-2 text-sm text-red-200">{error.message}</p></div>;
}

function FilterLabel({ label, children }: { label: string; children: ReactNode }) {
  return <label className="grid gap-1 font-mono text-[11px] font-bold uppercase text-slate-500">{label}{children}</label>;
}

function Meta({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return <div className="min-w-0"><dt className="font-mono text-[10px] font-bold uppercase text-slate-600">{label}</dt><dd className={`${mono ? "font-mono text-xs" : "text-sm"} mt-1 break-words text-slate-300`}>{value}</dd></div>;
}

function ScheduleLineage({ item }: { item: ExecutionAggregate }) {
  const provenance = scheduleProvenance(item);
  if (!provenance) return null;
  return <a href={`#schedules?id=${encodeURIComponent(provenance.scheduleId)}&trigger_id=${encodeURIComponent(provenance.triggerId)}`} className="w-fit rounded-sm font-mono text-xs text-cyan-300 hover:text-cyan-100 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300">Schedule {provenance.scheduleId} → Trigger {provenance.triggerId} → Execution</a>;
}

function executionFilters(params: URLSearchParams, context?: ExecutionViewContext): ExecutionFilters {
  const filters: ExecutionFilters = {};
  for (const key of ["state", "trigger", "created_after_unix_ms", "created_before_unix_ms", "search"] as const) {
    const value = params.get(key);
    if (value) filters[key] = value;
  }
  if (context?.actionId) filters.action_id = context.actionId;
  else if (params.get("action_id")) filters.action_id = params.get("action_id")!;
  const revision = context?.withinAction ? params.get("revision") ?? context.actionRevision : params.get("action_revision") ?? context?.actionRevision;
  if (revision) filters.action_revision = revision;
  return filters;
}

function executionHref(item: ExecutionAggregate, params: URLSearchParams, context?: ExecutionViewContext) {
  const next = new URLSearchParams(params);
  next.delete("id");
  next.delete("execution_id");
  next.set(context?.withinAction ? "execution_id" : "id", item.execution_id);
  if (context?.withinAction && context.actionId) {
    next.set("action_id", context.actionId);
    next.set("tab", "executions");
  }
  return hashFor(next, context);
}

function logsHref(execution: ExecutionAggregate, attemptId?: string, context?: ExecutionViewContext) {
  if (context?.withinAction && context.actionId) return actionHref(context.actionId, "logs", { revision: execution.action_revision, execution_id: execution.execution_id, attempt_id: attemptId });
  const query = new URLSearchParams({ execution_id: execution.execution_id });
  if (attemptId) query.set("attempt_id", attemptId);
  return `#logs?${query}`;
}

function runtimeHostHref(execution: ExecutionAggregate, runtimeHostId: string, attemptId?: string, context?: ExecutionViewContext) {
  if (context?.withinAction && context.actionId) return actionHref(context.actionId, "logs", { revision: execution.action_revision, execution_id: execution.execution_id, attempt_id: attemptId, runtime_host_id: runtimeHostId });
  const query = new URLSearchParams({ execution_id: execution.execution_id, runtime_host_id: runtimeHostId });
  if (attemptId) query.set("attempt_id", attemptId);
  return `#logs?${query}`;
}

function actionHref(actionId: string, tab: "executions" | "logs", values: Record<string, string | undefined> = {}) {
  const query = new URLSearchParams({ action_id: actionId, tab });
  for (const [key, value] of Object.entries(values)) if (value) query.set(key, value);
  return `#actions?${query}`;
}

function navigate(params: URLSearchParams, context?: ExecutionViewContext) { window.location.hash = hashFor(params, context).slice(1); }
function hashFor(params: URLSearchParams, context?: ExecutionViewContext) { return `#${context?.withinAction ? "actions" : "execution-preview"}${params.size ? `?${params}` : ""}`; }
function setParam(params: URLSearchParams, key: string, value: FormDataEntryValue | null) { const text = String(value ?? "").trim(); text ? params.set(key, text) : params.delete(key); }
function setDateParam(params: URLSearchParams, key: string, value: FormDataEntryValue | null) { const text = String(value ?? ""); const milliseconds = text ? new Date(text).getTime() : NaN; Number.isFinite(milliseconds) ? params.set(key, String(milliseconds)) : params.delete(key); }
function dateInputValue(value: string | null) { if (!value) return ""; const date = new Date(Number(value)); if (Number.isNaN(date.getTime())) return ""; const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000); return local.toISOString().slice(0, 16); }

function scheduleProvenance(item: ExecutionAggregate) {
  const source = item.trigger.type === "schedule" ? item.trigger : item.trigger.type === "manual" && item.trigger.source?.type === "schedule" ? item.trigger.source : undefined;
  return source?.schedule_id && source.trigger_id ? { scheduleId: source.schedule_id, triggerId: source.trigger_id } : undefined;
}

function latestDuration(item: ExecutionAggregate) { for (let index = item.attempts.length - 1; index >= 0; index -= 1) { const duration = item.attempts[index].result?.duration; if (duration) return formatDuration(duration); } return "—"; }
function executionStartedAt(item: ExecutionAggregate) { return item.attempts.find((attempt) => attempt.started_at)?.started_at ?? item.created_at; }
function executionFinishedAt(item: ExecutionAggregate) { for (let index = item.attempts.length - 1; index >= 0; index -= 1) if (item.attempts[index].finished_at) return item.attempts[index].finished_at; return item.terminal_state?.accepted_at; }
function formatDuration(duration: { secs: number; nanos: number }) { const milliseconds = duration.secs * 1000 + duration.nanos / 1_000_000; return milliseconds < 1 ? `${milliseconds.toFixed(2)}ms` : `${Math.round(milliseconds)}ms`; }
function timedOut(item: ExecutionAggregate) { return item.terminal_state?.state === "timed_out" || item.attempts.some((attempt) => attempt.outcome === "timed_out"); }
function formatTimestamp(value: unknown) { if (value == null) return "Unavailable"; if (typeof value === "string") return formatDate(new Date(value)); if (typeof value !== "object") return "Unavailable"; const timestamp = value as Record<string, unknown>; return typeof timestamp.secs_since_epoch === "number" && typeof timestamp.nanos_since_epoch === "number" ? formatDate(new Date(timestamp.secs_since_epoch * 1000 + timestamp.nanos_since_epoch / 1_000_000)) : "Unavailable"; }
function formatDate(date: Date) { return Number.isNaN(date.getTime()) ? "Unavailable" : date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" }); }
function labelState(value: string) { return value.split("_").map((part) => `${part.charAt(0).toUpperCase()}${part.slice(1)}`).join(" "); }
function triggerTone(type: string): "blue" | "cyan" | "slate" { return type === "schedule" ? "cyan" : type === "manual" ? "blue" : "slate"; }
function stateTone(state: string): "green" | "red" | "amber" | "blue" | "slate" { return state === "succeeded" ? "green" : ["failed", "cancelled", "timed_out", "infrastructure_failed"].includes(state) ? "red" : state === "cancellation_requested" ? "amber" : state === "running" ? "blue" : "slate"; }
function errorMessage(error: unknown) { return error instanceof Error ? error.message : "Request failed"; }

const controlClass = "min-h-[42px] w-full rounded-md border border-white/10 bg-[#08090b] px-3 text-sm text-slate-100 outline-none focus:border-violet-400/70 focus:ring-1 focus:ring-violet-400/30";
