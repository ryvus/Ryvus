import { useState } from "react";
import type { Artifacts, ScheduleArtifact } from "../artifacts/types";

type RunResult = {
  invocation_id?: string;
  status?: string;
  output?: unknown;
  error?: string;
  message?: string;
};

export function Schedules({ artifacts }: { artifacts: Artifacts }) {
  const [running, setRunning] = useState("");
  const [results, setResults] = useState<Record<string, RunResult>>({});

  async function runSchedule(schedule: ScheduleArtifact) {
    const id = schedule.id ?? schedule.name;
    setRunning(id);
    setResults((current) => ({ ...current, [id]: {} }));

    try {
      const response = await fetch(`/internal/scheduler/schedules/${encodeURIComponent(id)}/run`, {
        method: "POST",
      });
      const body = (await response.json()) as RunResult;
      setResults((current) => ({
        ...current,
        [id]: response.ok
          ? body
          : { error: body.error ?? "schedule_run_failed", message: body.message },
      }));
    } catch (error) {
      setResults((current) => ({
        ...current,
        [id]: {
          error: "request_failed",
          message: error instanceof Error ? error.message : "request failed",
        },
      }));
    } finally {
      setRunning("");
    }
  }

  return (
    <div className="page">
      <div className="section-heading">
        <span className="eyebrow">Runtime control</span>
        <h1>Schedules</h1>
      </div>
      <div className="schedule-list">
        {artifacts.schedules.schedules.map((schedule) => {
          const id = schedule.id ?? schedule.name;
          const result = results[id];

          return (
            <section className="schedule-item" key={schedule.action}>
              <div className="schedule-main">
                <div>
                  <h2>{schedule.name}</h2>
                  <p>
                    <code>{schedule.expression}</code>
                    <span>{schedule.enabled ? "enabled" : "disabled"}</span>
                  </p>
                </div>
                <button
                  type="button"
                  onClick={() => runSchedule(schedule)}
                  disabled={running === id}
                >
                  {running === id ? "Running..." : "Run now"}
                </button>
              </div>
              <dl className="schedule-meta">
                <div>
                  <dt>Runtime</dt>
                  <dd>{schedule.runtime}</dd>
                </div>
                <div>
                  <dt>Handler</dt>
                  <dd>
                    <code>{schedule.handler}</code>
                  </dd>
                </div>
                <div>
                  <dt>Run id</dt>
                  <dd>
                    <code>{id}</code>
                  </dd>
                </div>
              </dl>
              {result && (
                <div className={result.error ? "schedule-result error" : "schedule-result"}>
                  {result.error ? (
                    <>
                      <strong>{result.error}</strong>
                      <pre>{result.message ?? "Schedule run failed."}</pre>
                    </>
                  ) : result.invocation_id ? (
                    <>
                      <div className="response-meta">
                        <span>{result.status}</span>
                        <span>{result.invocation_id}</span>
                      </div>
                      <pre>{JSON.stringify(result.output ?? null, null, 2)}</pre>
                    </>
                  ) : (
                    <pre>Waiting for result...</pre>
                  )}
                </div>
              )}
            </section>
          );
        })}
      </div>
    </div>
  );
}
