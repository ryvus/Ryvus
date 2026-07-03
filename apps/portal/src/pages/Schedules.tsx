import { useState } from "react";
import type { Artifacts, ScheduleArtifact } from "../artifacts/types";
import { Badge, Button, CodeBlock, EmptyState, Page, Panel } from "../components/ui";

type RunResult = {
  invocation_id?: string;
  status?: string;
  output?: unknown;
  error?: string;
  message?: string;
};

type RunHistoryItem = RunResult & {
  id: string;
  started_at: string;
};

export function Schedules({ artifacts }: { artifacts: Artifacts }) {
  const [running, setRunning] = useState("");
  const [results, setResults] = useState<Record<string, RunResult>>({});
  const [history, setHistory] = useState<Record<string, RunHistoryItem[]>>({});
  const [selectedScheduleId, setSelectedScheduleId] = useState("");
  const schedules = artifacts.schedules.schedules;
  const selectedSchedule = schedules.find((schedule) => scheduleId(schedule) === selectedScheduleId);

  async function runSchedule(schedule: ScheduleArtifact) {
    const id = scheduleId(schedule);
    setRunning(id);
    setResults((current) => ({ ...current, [id]: {} }));

    try {
      const response = await fetch(`/internal/scheduler/schedules/${encodeURIComponent(id)}/run`, {
        method: "POST",
      });
      const body = (await response.json()) as RunResult;
      const result = response.ok
        ? body
        : { error: body.error ?? "schedule_run_failed", message: body.message };
      setResults((current) => ({
        ...current,
        [id]: result,
      }));
      recordRun(id, result);
    } catch (error) {
      const result = {
        error: "request_failed",
        message: error instanceof Error ? error.message : "request failed",
      };
      setResults((current) => ({ ...current, [id]: result }));
      recordRun(id, result);
    } finally {
      setRunning("");
    }
  }

  function recordRun(id: string, result: RunResult) {
    setHistory((current) => ({
      ...current,
      [id]: [
        {
          ...result,
          id: result.invocation_id ?? `local_${Date.now().toString(36)}`,
          started_at: new Date().toISOString(),
        },
        ...(current[id] ?? []),
      ],
    }));
  }

  return (
    <Page
      eyebrow="Runtime Control"
      title={selectedSchedule ? selectedSchedule.name : "Schedules"}
      actions={
        selectedSchedule && (
          <Button type="button" className="bg-white/10 hover:bg-white/15" onClick={() => setSelectedScheduleId("")}>
            Back to schedules
          </Button>
        )
      }
    >
      {schedules.length === 0 ? (
        <EmptyState title="No schedules" message="No scheduled actions were found in this artifact snapshot." />
      ) : selectedSchedule ? (
        <ScheduleDetail
          schedule={selectedSchedule}
          running={running === scheduleId(selectedSchedule)}
          result={results[scheduleId(selectedSchedule)]}
          history={history[scheduleId(selectedSchedule)] ?? []}
          onRun={() => runSchedule(selectedSchedule)}
        />
      ) : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {schedules.map((schedule) => (
            <ScheduleListItem
              key={scheduleId(schedule)}
              schedule={schedule}
              result={results[scheduleId(schedule)]}
              historyCount={(history[scheduleId(schedule)] ?? []).length}
              running={running === scheduleId(schedule)}
              onOpen={() => setSelectedScheduleId(scheduleId(schedule))}
            />
          ))}
        </div>
      )}
    </Page>
  );
}

function ScheduleListItem({
  schedule,
  result,
  historyCount,
  running,
  onOpen,
}: {
  schedule: ScheduleArtifact;
  result?: RunResult;
  historyCount: number;
  running: boolean;
  onOpen: () => void;
}) {
  return (
    <Panel className="grid content-between gap-5 p-4">
      <div className="grid gap-3">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <h2 className="truncate text-base font-semibold text-white">{schedule.name}</h2>
            <code className="mt-1 block truncate text-xs text-slate-500">{schedule.handler}</code>
          </div>
          <Badge tone={schedule.enabled ? "cyan" : "slate"}>{schedule.enabled ? "enabled" : "disabled"}</Badge>
        </div>
        <div className="grid grid-cols-3 gap-2">
          <Metric label="Every" value={schedule.expression} />
          <Metric label="Runtime" value={schedule.runtime} />
          <Metric label="Runs" value={historyCount.toString()} />
        </div>
        <Badge tone={running ? "blue" : result?.error ? "red" : result?.invocation_id ? "green" : "slate"}>
          {running ? "running" : resultStatus(result)}
        </Badge>
      </div>
      <Button type="button" onClick={onOpen}>Open schedule</Button>
    </Panel>
  );
}

function ScheduleDetail({
  schedule,
  running,
  result,
  history,
  onRun,
}: {
  schedule: ScheduleArtifact;
  running: boolean;
  result?: RunResult;
  history: RunHistoryItem[];
  onRun: () => void;
}) {
  const id = scheduleId(schedule);

  return (
    <div className="grid min-w-0 items-start gap-4 xl:grid-cols-[340px_minmax(0,1fr)]">
      <Panel className="grid gap-3 p-4">
        <div className="flex items-center justify-between gap-3">
          <h3 className="text-sm font-semibold text-white">Execution history</h3>
          <Badge tone="slate">{history.length}</Badge>
        </div>
        {history.length === 0 ? (
          <p className="text-sm text-slate-400">No manual schedule runs in this Portal session.</p>
        ) : (
          <div className="divide-y divide-white/10 rounded-lg border border-white/10 bg-black/20">
            {history.map((run) => (
              <div key={run.id} className="grid gap-2 px-3 py-3 md:grid-cols-[minmax(0,1fr)_auto] md:items-center">
                <span className="min-w-0">
                  <code className="block truncate text-xs text-slate-300">{run.id}</code>
                  <span className="text-xs text-slate-500">{new Date(run.started_at).toLocaleString()}</span>
                </span>
                <Badge tone={run.error ? "red" : run.invocation_id ? "green" : "slate"}>{resultStatus(run)}</Badge>
              </div>
            ))}
          </div>
        )}
      </Panel>
      <div className="grid min-w-0 gap-4">
        <Panel className="grid gap-4 p-4">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div>
              <h2 className="text-base font-semibold text-white">{schedule.name}</h2>
              <div className="mt-2 flex flex-wrap gap-2">
                <Badge tone="blue">{schedule.expression}</Badge>
                <Badge tone={schedule.enabled ? "cyan" : "slate"}>
                  {schedule.enabled ? "enabled" : "disabled"}
                </Badge>
              </div>
            </div>
            <Button type="button" onClick={onRun} disabled={running}>
              {running ? "Running..." : "Run now"}
            </Button>
          </div>
          <dl className="grid gap-3 sm:grid-cols-3">
            <div>
              <dt className="text-xs font-semibold uppercase text-slate-500">Runtime</dt>
              <dd className="mt-1 text-sm text-slate-200">{schedule.runtime}</dd>
            </div>
            <div>
              <dt className="text-xs font-semibold uppercase text-slate-500">Handler</dt>
              <dd>
                <code className="mt-1 block truncate text-sm text-slate-300">{schedule.handler}</code>
              </dd>
            </div>
            <div>
              <dt className="text-xs font-semibold uppercase text-slate-500">Run id</dt>
              <dd>
                <code className="mt-1 block truncate text-sm text-slate-300">{id}</code>
              </dd>
            </div>
          </dl>
        </Panel>

        <Panel className="grid gap-3 p-4">
          <div className="flex items-center justify-between gap-3">
            <h3 className="text-sm font-semibold text-white">Last run result</h3>
            <Badge tone={result?.error ? "red" : result?.invocation_id ? "green" : "slate"}>
              {running ? "running" : resultStatus(result)}
            </Badge>
          </div>
          {result ? (
            result.error ? (
              <>
                <strong className="text-sm text-red-200">{result.error}</strong>
                <CodeBlock>{result.message ?? "Schedule run failed."}</CodeBlock>
              </>
            ) : result.invocation_id ? (
              <>
                <div className="flex flex-wrap gap-3 text-sm font-medium text-emerald-300">
                  <span>{result.status}</span>
                  <span>{result.invocation_id}</span>
                </div>
                <CodeBlock>{JSON.stringify(result.output ?? null, null, 2)}</CodeBlock>
              </>
            ) : (
              <CodeBlock>Waiting for result...</CodeBlock>
            )
          ) : (
            <p className="text-sm text-slate-400">Run this schedule manually to preview its execution result.</p>
          )}
        </Panel>
      </div>
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg border border-white/10 bg-black/20 p-3">
      <div className="text-[11px] font-semibold uppercase text-slate-500">{label}</div>
      <div className="mt-1 truncate text-sm font-semibold text-slate-100">{value}</div>
    </div>
  );
}

function scheduleId(schedule: ScheduleArtifact) {
  return schedule.id ?? schedule.name;
}

function resultStatus(result?: RunResult) {
  if (!result) {
    return "none";
  }
  if (result.error) {
    return "failed";
  }
  if (result.invocation_id) {
    return result.status ?? "done";
  }
  return "waiting";
}
