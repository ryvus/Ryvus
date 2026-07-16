import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { historyApi } from "../api/history";
import { Badge, CodeBlock, EmptyState, Page, Panel } from "../components/ui";

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
  const executions = useQuery({ queryKey: ["executions", actionId, actionRevision], queryFn: () => historyApi.executions(actionId, actionRevision), enabled: !id });

  if (id) {
    if (execution.isError) return <EmptyState title="Execution unavailable" message={errorMessage(execution.error)} />;
    if (!execution.data) return <EmptyState title="Loading execution" message={id} />;
    const item = execution.data;
    return <Page eyebrow="Execution History" title={item.execution_id}><div className="grid gap-4"><Panel className="grid gap-3 p-4"><div className="flex flex-wrap gap-2"><Badge tone={item.terminal_state ? "green" : "blue"}>{item.state}</Badge><Badge tone="slate">{item.trigger.type}</Badge><Badge tone="slate">{item.action_revision}</Badge></div><CodeBlock>{JSON.stringify(item.trigger, null, 2)}</CodeBlock></Panel>{item.attempts.map((attempt) => <Panel key={attempt.attempt.attempt_id} className="grid gap-3 p-4"><div className="flex justify-between"><h2 className="font-semibold">Attempt {attempt.attempt.attempt_number}</h2><Badge tone={attempt.state === "failed" ? "red" : "green"}>{attempt.state}</Badge></div>{attempt.result?.events?.map((event, index) => <CodeBlock key={`${attempt.attempt.attempt_id}-${index}`}>{event.message ?? JSON.stringify(event)}</CodeBlock>)}{attempt.result && <CodeBlock>{JSON.stringify(attempt.result.invocation_result, null, 2)}</CodeBlock>}</Panel>)}</div></Page>;
  }

  if (executions.isError) return <EmptyState title="Execution history unavailable" message={errorMessage(executions.error)} />;
  return <Page eyebrow="Execution History" title={actionId ? `Executions for ${actionId}` : "Executions"}>{executions.data?.length ? <div className="grid gap-3">{executions.data.map((item) => <a key={item.execution_id} href={`#execution-preview?id=${encodeURIComponent(item.execution_id)}`}><Panel className="flex items-center justify-between gap-4 p-4"><span className="min-w-0"><code className="block truncate text-sm">{item.execution_id}</code><span className="text-xs text-slate-500">{item.action_id} · {item.action_revision}</span></span><Badge tone={item.terminal_state ? "green" : "blue"}>{item.state}</Badge></Panel></a>)}</div> : <EmptyState title="No executions" message="No matching durable executions were found." />}</Page>;
}

function errorMessage(error: unknown) { return error instanceof Error ? error.message : "Request failed"; }
