import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { historyApi, type ExecutionAggregate } from "../api/history";
import { Badge, Button, CodeBlock, EmptyState, Page, Panel } from "../components/ui";

export function Executions() {
  const [hash, setHash] = useState(window.location.hash);
  useEffect(() => {
    const update = () => setHash(window.location.hash);
    window.addEventListener("hashchange", update);
    return () => window.removeEventListener("hashchange", update);
  }, []);
  const params = new URLSearchParams(hash.split("?")[1] ?? "");
  const id = params.get("id");
  const actionId = params.get("action_id") ?? undefined;
  const actionRevision = params.get("action_revision") ?? undefined;
  const execution = useQuery({ queryKey: ["execution", id], queryFn: () => historyApi.execution(id!), enabled: Boolean(id) });
  const executions = useInfiniteQuery({
    queryKey: ["executions", actionId, actionRevision],
    queryFn: ({ pageParam }) => historyApi.executions(actionId, actionRevision, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: !id,
  });
  const executionItems = executions.data?.pages.flatMap((page) => page.items) ?? [];

  if (id) {
    if (execution.isError && !execution.data) return <EmptyState title="Execution unavailable" message={errorMessage(execution.error)} />;
    if (!execution.data) return <EmptyState title="Loading execution" message={id} />;
    const item = execution.data;
    return (
      <Page
        eyebrow="Execution History"
        title={item.execution_id}
        actions={<LogLink executionId={item.execution_id}>View execution logs</LogLink>}
      >
        {execution.error && <p className="text-sm text-red-300">{errorMessage(execution.error)}</p>}
        <div className="grid gap-4">
          <Panel className="grid gap-3 p-4">
            <div className="flex flex-wrap gap-2">
              <Badge tone={stateTone(item.state)}>{item.state}</Badge>
              <Badge tone={triggerTone(item.trigger.type)}>{item.trigger.type}</Badge>
              <Badge tone="slate">{item.action_revision}</Badge>
            </div>
            <ScheduleLineage item={item} />
            <CodeBlock>{JSON.stringify(item.trigger, null, 2)}</CodeBlock>
          </Panel>
          {item.attempts.map((attempt) => (
            <Panel key={attempt.attempt.attempt_id} className="grid gap-3 p-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <h2 className="font-semibold">Attempt {attempt.attempt.attempt_number}</h2>
                <span className="flex flex-wrap items-center justify-end gap-2">
                  <LogLink executionId={item.execution_id} attemptId={attempt.attempt.attempt_id}>View logs</LogLink>
                  <span className="font-mono text-xs text-slate-500">{attempt.result ? formatDuration(attempt.result.duration) : "—"}</span>
                  <Badge tone={stateTone(attempt.state)}>{attempt.state}</Badge>
                </span>
              </div>
              {attempt.result && <CodeBlock>{JSON.stringify(attempt.result.invocation_result, null, 2)}</CodeBlock>}
            </Panel>
          ))}
        </div>
      </Page>
    );
  }

  if (executions.isError && !executions.data) return <EmptyState title="Execution history unavailable" message={errorMessage(executions.error)} />;
  if (!executions.data) return <EmptyState title="Loading executions" message="Reading durable execution state." />;
  return <Page eyebrow="Execution History" title={actionId ? `Executions for ${actionId}` : "Executions"}>{executions.error && <p className="text-sm text-red-300">{errorMessage(executions.error)}</p>}{executionItems.length ? <div className="grid gap-3">{executionItems.map((item) => <Panel key={item.execution_id} className="grid gap-3 p-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center"><div className="grid min-w-0 gap-2"><a href={`#execution-preview?id=${encodeURIComponent(item.execution_id)}`} className="min-w-0 rounded-sm focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300"><code className="block truncate text-sm text-slate-100">{item.execution_id}</code><span className="text-xs text-slate-500">{item.action_id} · {item.action_revision}</span></a><ScheduleLineage item={item} /></div><div className="flex flex-wrap items-center gap-2 sm:justify-end"><Badge tone={triggerTone(item.trigger.type)}>{item.trigger.type}</Badge><Badge tone={stateTone(item.state)}>{item.state}</Badge><span className="flex items-center gap-1 text-xs text-slate-500">Terminal <Badge tone={item.terminal_state ? stateTone(item.terminal_state.state) : "slate"}>{item.terminal_state?.state ?? "—"}</Badge></span><span className="font-mono text-xs text-slate-500">{item.attempts.length} attempt{item.attempts.length === 1 ? "" : "s"} · {latestDuration(item)}</span></div></Panel>)}{executions.hasNextPage && <Button type="button" className="justify-self-start" onClick={() => void executions.fetchNextPage()} disabled={executions.isFetchingNextPage}>Load more</Button>}</div> : <EmptyState title="No executions" message="No matching durable executions were found." />}</Page>;
}

function LogLink({ executionId, attemptId, children }: { executionId: string; attemptId?: string; children: string }) {
  const query = new URLSearchParams({ execution_id: executionId });
  if (attemptId) query.set("attempt_id", attemptId);
  return <a href={`#logs?${query}`} className="inline-flex min-h-9 items-center rounded-md border border-white/10 px-3 font-mono text-xs font-bold text-violet-200 hover:border-violet-300/30 hover:bg-violet-500/10 hover:text-white focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300">{children}</a>;
}

function ScheduleLineage({ item }: { item: ExecutionAggregate }) {
  const provenance = scheduleProvenance(item);
  if (!provenance) return null;
  return <a href={`#schedules?id=${encodeURIComponent(provenance.scheduleId)}&trigger_id=${encodeURIComponent(provenance.triggerId)}`} className="w-fit rounded-sm font-mono text-xs text-cyan-300 hover:text-cyan-100 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300">Schedule {provenance.scheduleId} → Trigger {provenance.triggerId} → Execution</a>;
}

function scheduleProvenance(item: ExecutionAggregate) {
  const source = item.trigger.type === "schedule" ? item.trigger : item.trigger.type === "manual" && item.trigger.source?.type === "schedule" ? item.trigger.source : undefined;
  return source?.schedule_id && source.trigger_id ? { scheduleId: source.schedule_id, triggerId: source.trigger_id } : undefined;
}

function latestDuration(item: ExecutionAggregate) {
  for (let index = item.attempts.length - 1; index >= 0; index -= 1) {
    const duration = item.attempts[index].result?.duration;
    if (duration) return formatDuration(duration);
  }
  return "—";
}

function formatDuration(duration: { secs: number; nanos: number }) {
  const milliseconds = duration.secs * 1000 + duration.nanos / 1_000_000;
  return milliseconds < 1 ? `${milliseconds.toFixed(2)}ms` : `${Math.round(milliseconds)}ms`;
}

function triggerTone(type: string): "blue" | "cyan" | "slate" { return type === "schedule" ? "cyan" : type === "manual" ? "blue" : "slate"; }
function stateTone(state: string): "green" | "red" | "amber" | "blue" | "slate" { return state === "succeeded" ? "green" : ["failed", "cancelled", "timed_out"].includes(state) ? "red" : state === "cancellation_requested" ? "amber" : state === "running" ? "blue" : "slate"; }

function errorMessage(error: unknown) { return error instanceof Error ? error.message : "Request failed"; }
