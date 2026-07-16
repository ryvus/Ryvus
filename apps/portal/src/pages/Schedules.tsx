import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import type { Artifacts } from "../artifacts/types";
import { historyApi, type ScheduleRecord } from "../api/history";
import { Badge, Button, EmptyState, Page, Panel } from "../components/ui";

export function Schedules({ artifacts: _artifacts }: { artifacts: Artifacts }) {
  const client = useQueryClient();
  const [selectedId, setSelectedId] = useState("");
  const schedules = useQuery({ queryKey: ["schedules"], queryFn: historyApi.schedules });
  const selected = schedules.data?.find((schedule) => schedule.schedule_id === selectedId);
  const triggers = useQuery({
    queryKey: ["schedule-triggers", selectedId],
    queryFn: () => historyApi.triggers(selectedId),
    enabled: Boolean(selectedId),
  });
  const refresh = async () => {
    await client.invalidateQueries({ queryKey: ["schedules"] });
    await client.invalidateQueries({ queryKey: ["schedule-triggers", selectedId] });
    await client.invalidateQueries({ queryKey: ["executions"] });
  };
  const run = useMutation({ mutationFn: () => historyApi.run(selectedId), onSuccess: refresh });
  const toggle = useMutation({
    mutationFn: (schedule: ScheduleRecord) => schedule.enablement === "enabled" ? historyApi.disable(schedule.schedule_id) : historyApi.enable(schedule.schedule_id),
    onSuccess: refresh,
  });

  if (schedules.isError) return <EmptyState title="Schedule history unavailable" message={errorMessage(schedules.error)} />;
  if (!schedules.data) return <EmptyState title="Loading schedules" message="Reading durable Scheduler state." />;

  return (
    <Page eyebrow="Runtime Control" title={selected?.display_name ?? "Schedules"} actions={selected && <Button type="button" onClick={() => setSelectedId("")}>Back</Button>}>
      {schedules.data.length === 0 ? <EmptyState title="No schedules" message="No durable schedules were discovered." /> : selected ? (
        <div className="grid gap-4 xl:grid-cols-[340px_minmax(0,1fr)]">
          <Panel className="grid gap-3 p-4 self-start">
            <div className="flex items-center justify-between"><h2 className="font-semibold">Trigger history</h2><Badge tone="slate">{triggers.data?.length ?? 0}</Badge></div>
            {(triggers.data ?? []).map((trigger) => (
              <a key={trigger.trigger_id} href={trigger.execution_id ? `#execution-preview?id=${encodeURIComponent(trigger.execution_id)}` : undefined} className="grid gap-1 rounded-lg border border-white/10 p-3 hover:bg-white/[0.04]">
                <span className="flex justify-between gap-2"><Badge tone={trigger.kind === "manual" ? "blue" : "cyan"}>{trigger.kind}</Badge><Badge tone={trigger.status === "failed" ? "red" : trigger.status === "execution_created" ? "green" : "slate"}>{trigger.status}</Badge></span>
                <code className="truncate text-xs text-slate-400">{trigger.execution_id ?? trigger.trigger_id}</code>
                <span className="text-xs text-slate-500">revision {trigger.schedule_revision} · {trigger.action_revision}</span>
              </a>
            ))}
            {!triggers.isLoading && triggers.data?.length === 0 && <p className="text-sm text-slate-400">No triggers yet.</p>}
          </Panel>
          <Panel className="grid gap-4 p-4 self-start">
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div><h2 className="text-lg font-semibold">{selected.display_name}</h2><code className="text-xs text-slate-500">{selected.stable_schedule_key}</code></div>
              <div className="flex gap-2"><Button type="button" onClick={() => toggle.mutate(selected)}>{selected.enablement === "enabled" ? "Disable" : "Enable"}</Button><Button type="button" onClick={() => run.mutate()} disabled={run.isPending || selected.availability === "unavailable"}>{run.isPending ? "Running..." : "Run now"}</Button></div>
            </div>
            <div className="flex flex-wrap gap-2"><Badge tone={selected.availability === "available" ? "green" : "red"}>{selected.availability}</Badge><Badge tone={selected.enablement === "enabled" ? "cyan" : "slate"}>{selected.enablement}</Badge><Badge tone="blue">revision {selected.current_revision}</Badge></div>
            <dl className="grid gap-3 sm:grid-cols-2"><Metric label="Next trigger" value={timeLabel(selected.next_trigger_at)} /><Metric label="Last trigger" value={timeLabel(selected.last_scheduled_trigger_at)} /></dl>
            {(run.error || toggle.error) && <p className="text-sm text-red-300">{errorMessage(run.error ?? toggle.error)}</p>}
          </Panel>
        </div>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">{schedules.data.map((schedule) => <button key={schedule.schedule_id} type="button" className="text-left" onClick={() => setSelectedId(schedule.schedule_id)}><Panel className="grid gap-3 p-4"><div className="flex justify-between gap-3"><h2 className="font-semibold">{schedule.display_name}</h2><Badge tone={schedule.availability === "available" ? "green" : "red"}>{schedule.availability}</Badge></div><code className="truncate text-xs text-slate-500">{schedule.stable_schedule_key}</code><div className="flex gap-2"><Badge tone={schedule.enablement === "enabled" ? "cyan" : "slate"}>{schedule.enablement}</Badge><Badge tone="blue">revision {schedule.current_revision}</Badge></div></Panel></button>)}</div>
      )}
    </Page>
  );
}

function Metric({ label, value }: { label: string; value: string }) { return <div className="rounded-lg border border-white/10 bg-black/20 p-3"><dt className="text-xs uppercase text-slate-500">{label}</dt><dd className="mt-1 text-sm text-slate-200">{value}</dd></div>; }
function timeLabel(value: unknown) { return value ? JSON.stringify(value) : "—"; }
function errorMessage(error: unknown) { return error instanceof Error ? error.message : "Request failed"; }
