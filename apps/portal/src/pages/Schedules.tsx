import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import type { Artifacts } from "../artifacts/types";
import { historyApi, type ScheduleRecord, type ScheduleTrigger } from "../api/history";
import { actionHref } from "../artifacts/actions";
import { Badge, Button, EmptyState, Page, Panel, cn } from "../components/ui";

type TriggerKind = "all" | "scheduled" | "manual";

export function Schedules({ artifacts }: { artifacts: Artifacts }) {
  const client = useQueryClient();
  const [hash, setHash] = useState(window.location.hash);
  const [triggerKind, setTriggerKind] = useState<TriggerKind>("all");
  useEffect(() => {
    const update = () => setHash(window.location.hash);
    window.addEventListener("hashchange", update);
    return () => window.removeEventListener("hashchange", update);
  }, []);
  const params = new URLSearchParams(hash.split("?")[1] ?? "");
  const selectedId = params.get("id") ?? "";
  const selectedTriggerId = params.get("trigger_id");
  const schedules = useInfiniteQuery({
    queryKey: ["schedules"],
    queryFn: ({ pageParam }) => historyApi.schedules(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
  });
  const scheduleItems = schedules.data?.pages.flatMap((page) => page.items) ?? [];
  const listedSelection = scheduleItems.find((schedule) => schedule.schedule_id === selectedId);
  const selectedSchedule = useQuery({
    queryKey: ["schedule", selectedId],
    queryFn: () => historyApi.schedule(selectedId),
    enabled: Boolean(selectedId && !listedSelection),
  });
  const selected = listedSelection ?? selectedSchedule.data;
  const selectedAction = selected && artifacts.catalog.actions.find((action) => {
    const schedule = (action.kind as { Schedule?: { key?: string } }).Schedule;
    return schedule?.key === selected.stable_schedule_key;
  });
  const triggers = useInfiniteQuery({
    queryKey: ["schedule-triggers", selectedId, triggerKind],
    queryFn: ({ pageParam }) => historyApi.triggers(selectedId, triggerKind === "all" ? undefined : triggerKind, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: Boolean(selectedId),
  });
  const triggerItems = triggers.data?.pages.flatMap((page) => page.items) ?? [];
  const refresh = async () => {
    await client.invalidateQueries({ queryKey: ["schedules"] });
    await client.invalidateQueries({ queryKey: ["schedule", selectedId] });
    await client.invalidateQueries({ queryKey: ["schedule-triggers", selectedId] });
    await client.invalidateQueries({ queryKey: ["executions"] });
  };
  const run = useMutation({ mutationFn: () => historyApi.run(selectedId), onSuccess: refresh });
  const toggle = useMutation({
    mutationFn: (schedule: ScheduleRecord) => schedule.enablement === "enabled" ? historyApi.disable(schedule.schedule_id) : historyApi.enable(schedule.schedule_id),
    onSuccess: refresh,
  });

  if (!selectedId && schedules.isError && !schedules.data) return <EmptyState title="Schedule history unavailable" message={errorMessage(schedules.error)} />;
  if (!selectedId && !schedules.data) return <EmptyState title="Loading schedules" message="Reading durable Scheduler state." />;
  if (selectedId && !selected && selectedSchedule.isError) return <EmptyState title="Schedule unavailable" message={errorMessage(selectedSchedule.error)} />;
  if (selectedId && !selected) return <EmptyState title="Loading schedule" message={selectedId} />;

  return (
    <Page eyebrow="Runtime Control" title={selected?.display_name ?? "Schedules"} actions={selected && <Button type="button" onClick={() => { window.location.hash = "schedules"; }}>Back</Button>}>
      {(selectedSchedule.error || schedules.error) && <p className="text-sm text-red-300">{errorMessage(selectedSchedule.error ?? schedules.error)}</p>}
      {selected ? (
        <div className="grid gap-4 xl:grid-cols-[340px_minmax(0,1fr)]">
          <Panel className="grid gap-3 p-4 self-start">
            <div className="flex flex-wrap items-center justify-between gap-2"><h2 className="font-semibold">Trigger history</h2><Badge tone="slate">{triggerItems.length}</Badge></div>
            <label className="grid gap-1 font-mono text-[11px] font-bold uppercase text-slate-500">
              Trigger kind
              <select value={triggerKind} onChange={(event) => setTriggerKind(event.target.value as TriggerKind)} className="min-h-9 rounded-md border border-white/15 bg-[#090a0c] px-2 text-sm font-medium normal-case text-slate-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300">
                <option value="all">All</option>
                <option value="scheduled">Scheduled</option>
                <option value="manual">Manual</option>
              </select>
            </label>
            {triggerItems.map((trigger) => <TriggerCard key={trigger.trigger_id} trigger={trigger} selected={trigger.trigger_id === selectedTriggerId} />)}
            {triggers.isLoading && <p className="text-sm text-slate-400">Loading triggers…</p>}
            {triggers.error && <p className="text-sm text-red-300">{errorMessage(triggers.error)}</p>}
            {!triggers.isLoading && !triggers.error && triggerItems.length === 0 && <p className="text-sm text-slate-400">No triggers yet.</p>}
            {triggers.hasNextPage && <Button type="button" onClick={() => void triggers.fetchNextPage()} disabled={triggers.isFetchingNextPage}>Load more</Button>}
          </Panel>
          <Panel className="grid gap-4 p-4 self-start">
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div><h2 className="text-lg font-semibold">{selected.display_name}</h2><code className="text-xs text-slate-500">{selected.stable_schedule_key}</code></div>
              <div className="flex flex-wrap gap-2">
                {selectedAction && <a className="inline-flex min-h-9 items-center rounded-md border border-white/10 px-3 font-mono text-xs font-bold text-slate-300 hover:bg-white/[0.04] hover:text-white" href={actionHref(selectedAction)}>View action</a>}
                <Button type="button" onClick={() => toggle.mutate(selected)}>{selected.enablement === "enabled" ? "Disable" : "Enable"}</Button>
                <Button type="button" onClick={() => run.mutate()} disabled={run.isPending || selected.availability === "unavailable"}>{run.isPending ? "Running..." : "Run now"}</Button>
              </div>
            </div>
            <div className="flex flex-wrap gap-2"><Badge tone={selected.availability === "available" ? "green" : "red"}>{selected.availability}</Badge><Badge tone={selected.enablement === "enabled" ? "cyan" : "slate"}>{selected.enablement}</Badge><Badge tone="blue">revision {selected.current_revision}</Badge></div>
            <dl className="grid min-w-0 gap-3 sm:grid-cols-2"><Metric label="Next trigger" value={selected.next_trigger_at} /><Metric label="Last trigger" value={selected.last_scheduled_trigger_at} /></dl>
            {(run.error || toggle.error) && <p className="text-sm text-red-300">{errorMessage(run.error ?? toggle.error)}</p>}
          </Panel>
        </div>
      ) : scheduleItems.length === 0 ? <EmptyState title="No schedules" message="No durable schedules were discovered." /> : <div className="grid gap-4"><div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">{scheduleItems.map((schedule) => <a key={schedule.schedule_id} href={`#schedules?id=${encodeURIComponent(schedule.schedule_id)}`} className="group block text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300"><Panel className="grid gap-3 p-4 transition-colors group-hover:border-white/20 group-hover:bg-white/[0.04]"><div className="flex justify-between gap-3"><h2 className="font-semibold">{schedule.display_name}</h2><Badge tone={schedule.availability === "available" ? "green" : "red"}>{schedule.availability}</Badge></div><code className="truncate text-xs text-slate-500">{schedule.stable_schedule_key}</code><div className="flex gap-2"><Badge tone={schedule.enablement === "enabled" ? "cyan" : "slate"}>{schedule.enablement}</Badge><Badge tone="blue">revision {schedule.current_revision}</Badge></div></Panel></a>)}</div>{schedules.hasNextPage && <Button type="button" className="justify-self-start" onClick={() => void schedules.fetchNextPage()} disabled={schedules.isFetchingNextPage}>Load more</Button>}</div>}
    </Page>
  );
}

function TriggerCard({ trigger, selected }: { trigger: ScheduleTrigger; selected: boolean }) {
  const className = cn("grid gap-1 rounded-lg border p-3", selected ? "border-violet-400/50 bg-violet-500/10" : "border-white/10", trigger.execution_id && "hover:bg-white/[0.04] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300");
  const content = <><span className="flex justify-between gap-2"><Badge tone={trigger.kind === "manual" ? "blue" : "cyan"}>{trigger.kind}</Badge><Badge tone={trigger.status === "failed" ? "red" : trigger.status === "execution_created" ? "green" : "slate"}>{trigger.status}</Badge></span><code className="truncate text-xs text-slate-400">{trigger.execution_id ?? trigger.trigger_id}</code><span className="text-xs text-slate-500">revision {trigger.schedule_revision} · {trigger.action_revision}</span></>;
  return trigger.execution_id
    ? <a href={`#execution-preview?id=${encodeURIComponent(trigger.execution_id)}`} className={className} aria-current={selected ? "true" : undefined}>{content}</a>
    : <div className={className} aria-current={selected ? "true" : undefined}>{content}</div>;
}

function Metric({ label, value }: { label: string; value: unknown }) { return <div className="min-w-0 rounded-lg border border-white/10 bg-black/20 p-3"><dt className="text-xs uppercase text-slate-500">{label}</dt><dd className="mt-1 min-w-0 text-sm text-slate-200"><TimeValue value={value} /></dd></div>; }
function TimeValue({ value }: { value: unknown }) {
  if (value == null) return <>—</>;
  const date = timestampDate(value);
  if (!date) return <span className="block truncate" title="Malformed timestamp">Invalid timestamp</span>;
  const dateTime = date.toISOString();
  return <time className="block truncate" dateTime={dateTime} title={dateTime}>{date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" })}</time>;
}
function timestampDate(value: unknown) {
  if (typeof value === "string") {
    if (!value.trim()) return null;
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? null : date;
  }
  if (!value || typeof value !== "object") return null;
  const { secs_since_epoch: seconds, nanos_since_epoch: nanos } = value as Record<string, unknown>;
  if (typeof seconds !== "number" || typeof nanos !== "number" || !Number.isSafeInteger(seconds) || !Number.isInteger(nanos) || nanos < 0 || nanos >= 1_000_000_000) return null;
  const date = new Date(seconds * 1_000 + nanos / 1_000_000);
  return Number.isNaN(date.getTime()) ? null : date;
}
function errorMessage(error: unknown) { return error instanceof Error ? error.message : "Request failed"; }
