import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useState, type KeyboardEvent } from "react";
import { actionsApi, type ActionDetailDto, type ObservedRevision } from "../api/actions";
import { historyApi, type ExecutionAggregate } from "../api/history";
import {
  type ActionDefinition,
  type Artifacts,
  type CatalogAction,
  type EffectiveActionPolicy,
} from "../artifacts/types";
import { Badge, EmptyState, Page, Panel, cn } from "../components/ui";
import { Executions } from "./Executions";
import { Logs } from "./Logs";

const tabs = ["overview", "executions", "logs", "configuration", "revisions"] as const;
type ActionTab = (typeof tabs)[number];

export function Actions({ artifacts }: { artifacts: Artifacts }) {
  const [hash, setHash] = useState(window.location.hash);
  useEffect(() => {
    const update = () => setHash(window.location.hash);
    window.addEventListener("hashchange", update);
    return () => window.removeEventListener("hashchange", update);
  }, []);

  const params = new URLSearchParams(hash.split("?")[1] ?? "");
  const selectedActionId = params.get("action_id");
  return selectedActionId
    ? <ActionDetailPage actionId={selectedActionId} params={params} catalogAction={artifacts.catalog.actions.find((action) => actionId(action) === selectedActionId)} />
    : <ActionList actions={artifacts.catalog.actions} search={params.get("search") ?? ""} />;
}

function ActionList({ actions, search }: { actions: CatalogAction[]; search: string }) {
  const filtered = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    if (!query) return actions;
    return actions.filter((action) => [action.name, actionId(action), action.source, action.entrypoint]
      .some((value) => value?.toLocaleLowerCase().includes(query)));
  }, [actions, search]);

  function updateSearch(value: string) {
    const params = new URLSearchParams();
    if (value) params.set("search", value);
    window.location.hash = params.size ? `actions?${params}` : "actions";
  }

  return (
    <Page eyebrow="Discovered resources" title="Actions">
      <label className="grid max-w-xl gap-2 font-mono text-[11px] font-bold uppercase text-slate-500">
        Search actions
        <input
          type="search"
          value={search}
          placeholder="Name, key, source, or entrypoint"
          onChange={(event) => updateSearch(event.target.value)}
        />
      </label>
      {actions.length === 0 ? (
        <EmptyState title="No discovered actions" message="Run ryvus start to discover project actions and load the catalog." />
      ) : filtered.length === 0 ? (
        <EmptyState title="No matching actions" message={`No discovered action matches “${search}”.`} />
      ) : (
        <Panel className="overflow-x-auto">
          <table className="w-full min-w-[880px] border-collapse text-left text-sm">
            <thead className="border-b border-white/10 font-mono text-[11px] uppercase text-slate-500">
              <tr>
                <th className="px-4 py-3 font-bold">Action</th>
                <th className="px-4 py-3 font-bold">Action key</th>
                <th className="px-4 py-3 font-bold">Kind</th>
                <th className="px-4 py-3 font-bold">Runtime</th>
                <th className="px-4 py-3 font-bold">Current revision</th>
                <th className="px-4 py-3 font-bold">Discovery</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-white/[0.07]">
              {filtered.map((action) => {
                const id = actionId(action);
                return (
                  <tr key={`${id}:${action.entrypoint}`} className="transition-colors hover:bg-white/[0.035]">
                    <td className="p-0">
                      <a className="block min-h-11 px-4 py-3 font-semibold text-white focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-violet-300" href={`#actions?action_id=${encodeURIComponent(id)}`}>
                        {action.name ?? action.entrypoint}
                      </a>
                    </td>
                    <td className="px-4 py-3 font-mono text-xs text-slate-400">{id}</td>
                    <td className="px-4 py-3"><Badge tone="violet">{actionKindLabel(action)}</Badge></td>
                    <td className="px-4 py-3 text-slate-300">{action.runtime}</td>
                    <td className="px-4 py-3 font-mono text-xs text-slate-500">{action.action_revision}</td>
                    <td className="px-4 py-3"><Badge tone="green">Discovered</Badge></td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </Panel>
      )}
    </Page>
  );
}

function ActionDetailPage({ actionId: selectedActionId, params, catalogAction }: { actionId: string; params: URLSearchParams; catalogAction?: CatalogAction }) {
  const detail = useQuery({
    queryKey: ["action-detail", selectedActionId],
    queryFn: () => actionsApi.detail(selectedActionId),
  });
  const requestedTab = params.get("tab");
  const activeTab: ActionTab = tabs.includes(requestedTab as ActionTab) ? requestedTab as ActionTab : "overview";

  const backLink = "w-fit rounded-sm font-mono text-xs text-cyan-300 hover:text-cyan-100 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300";
  if (detail.isError) {
    return <Page eyebrow="Actions" title="Action unavailable"><a href="#actions" className={backLink}>← Actions</a><EmptyState title="Action unavailable" message={errorMessage(detail.error)} /></Page>;
  }
  if (!detail.data) {
    return <Page eyebrow="Actions" title="Loading action"><a href="#actions" className={backLink}>← Actions</a><EmptyState title="Loading action" message={selectedActionId} /></Page>;
  }

  const action = detail.data;

  function handleTabKeyDown(event: KeyboardEvent<HTMLAnchorElement>, tab: ActionTab) {
    if (event.key === " ") {
      event.preventDefault();
      event.currentTarget.click();
      return;
    }

    const currentIndex = tabs.indexOf(tab);
    const targetIndex = event.key === "ArrowLeft"
      ? (currentIndex - 1 + tabs.length) % tabs.length
      : event.key === "ArrowRight"
        ? (currentIndex + 1) % tabs.length
        : event.key === "Home"
          ? 0
          : event.key === "End"
            ? tabs.length - 1
            : -1;
    if (targetIndex < 0) return;

    event.preventDefault();
    const target = document.getElementById(`action-tab-${tabs[targetIndex]}`) as HTMLAnchorElement | null;
    target?.focus();
    target?.click();
  }

  return (
    <Page eyebrow="Action detail" title={action.display_name}>
      <a href="#actions" className={backLink}>← Actions</a>
      <Panel className="overflow-hidden">
        <div className="grid gap-4 p-4 sm:p-5 lg:grid-cols-[minmax(0,1fr)_auto] lg:items-start">
          <div className="min-w-0">
            <div className="mb-2 flex flex-wrap items-center gap-2">
              <Badge tone="violet">{actionKindLabel(action.definition)}</Badge>
              <Badge tone="green">Discovered</Badge>
            </div>
            <h2 className="truncate text-xl font-semibold tracking-tight text-white">{action.display_name}</h2>
            <p className="mt-1 truncate font-mono text-xs text-slate-500" title={action.action_id}>Action key · {action.action_id}</p>
          </div>
          <dl className="grid grid-cols-2 gap-x-6 gap-y-3 text-sm sm:grid-cols-3">
            <Meta label="Runtime" value={action.definition.runtime} />
            <Meta label="Current revision" value={action.current_revision} mono />
          </dl>
        </div>
        <nav className="overflow-x-auto border-t border-white/10" aria-label="Action detail" role="tablist">
          <div className="flex min-w-max px-2">
            {tabs.map((tab) => {
              return (
                <a
                  key={tab}
                  id={`action-tab-${tab}`}
                  href={actionTabHref(selectedActionId, tab, params)}
                  role="tab"
                  aria-selected={activeTab === tab}
                  aria-controls="action-panel"
                  tabIndex={activeTab === tab ? 0 : -1}
                  onKeyDown={(event) => handleTabKeyDown(event, tab)}
                  className={cn(
                    "inline-flex min-h-11 items-center border-b-2 border-transparent px-3 text-sm font-semibold capitalize text-slate-500 transition-colors hover:text-slate-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-violet-300",
                    activeTab === tab && "border-cyan-300 text-white",
                  )}
                >
                  {tab}
                </a>
              );
            })}
          </div>
        </nav>
      </Panel>
      <div id="action-panel" role="tabpanel" aria-labelledby={`action-tab-${activeTab}`} tabIndex={0} className="rounded-md focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300">
        {activeTab === "overview" && <OverviewTab detail={action} />}
        {activeTab === "executions" && <Executions context={{ actionId: action.action_id, actionRevision: params.get("revision") ?? undefined, executionId: params.get("execution_id") ?? undefined, withinAction: true }} />}
        {activeTab === "configuration" && <ConfigurationTab detail={action} effectivePolicy={catalogAction?.effective_policy} />}
        {activeTab === "revisions" && <RevisionsTab detail={action} selectedRevision={params.get("revision")} />}
        {activeTab === "logs" && <Logs context={{ actionId: action.action_id, actionRevision: params.get("revision") ?? action.current_revision, executionId: params.get("execution_id") ?? undefined, attemptId: params.get("attempt_id") ?? undefined, runtimeHostId: params.get("runtime_host_id") ?? undefined, withinAction: true }} />}
      </div>
    </Page>
  );
}

function OverviewTab({ detail }: { detail: ActionDetailDto }) {
  const schedule = useSchedule(scheduleStableKey(detail.definition));
  const health = detail.recent_health;

  return (
    <div className="grid gap-4">
      <Panel className="grid gap-5 p-4 sm:p-5 xl:grid-cols-[minmax(0,1fr)_minmax(280px,0.7fr)]">
        <section>
          <SectionHeading title="Action information" message="Current discovery metadata." />
          <dl className="mt-4 grid gap-x-6 gap-y-4 sm:grid-cols-2">
            <Meta label="Action key" value={detail.action_id} mono />
            <Meta label="Display name" value={detail.display_name} />
            <Meta label="Kind" value={actionKindLabel(detail.definition)} />
            <Meta label="Runtime" value={detail.definition.runtime} />
            <Meta label="Current revision" value={detail.current_revision} mono />
            <Meta label="Status" value="Discovered" />
            <Meta label="Source" value={detail.definition.source} mono />
            <Meta label="Entrypoint" value={detail.definition.entrypoint} mono />
          </dl>
        </section>
        <section className="border-t border-white/10 pt-5 xl:border-l xl:border-t-0 xl:pl-5 xl:pt-0">
          <SectionHeading title="Current trigger" message="Read-only trigger metadata and operational state where available." />
          <TriggerDetails definition={detail.definition} schedule={schedule} />
        </section>
      </Panel>

      <Panel className="overflow-hidden">
        <div className="border-b border-white/10 p-4 sm:p-5">
          <SectionHeading title="Recent health" message={`${health.sample_size} of the latest ${health.window} executions`} />
        </div>
        {health.sample_size === 0 ? (
          <div className="p-4 sm:p-5"><p className="text-sm text-slate-400">No execution data</p></div>
        ) : (
          <dl className="grid grid-cols-2 divide-x divide-y divide-white/10 sm:grid-cols-3 xl:grid-cols-6 xl:divide-y-0">
            <HealthMetric label={`Executions · latest ${health.window}`} value={String(health.sample_size)} />
            <HealthMetric label={`Success rate · latest ${health.window}`} value={formatPercent(health.success_rate)} />
            <HealthMetric label={`Failures · latest ${health.window}`} value={String(health.failed)} tone={health.failed > 0 ? "text-red-300" : undefined} />
            <HealthMetric label={`Average duration · latest ${health.window}`} value={formatMilliseconds(health.average_duration_ms)} />
            <HealthMetric label={`P95 duration · latest ${health.window}`} value={formatMilliseconds(health.p95_duration_ms)} />
            <HealthMetric label={`Active · latest ${health.window}`} value={String(health.active)} tone={health.active > 0 ? "text-blue-200" : undefined} />
          </dl>
        )}
      </Panel>

      <Panel className="overflow-hidden">
        <div className="border-b border-white/10 p-4 sm:p-5"><SectionHeading title="Recent executions" message="The five latest executions observed for this action." /></div>
        {detail.recent_executions.length === 0 ? <p className="p-4 text-sm text-slate-400 sm:p-5">No execution data</p> : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[780px] border-collapse text-left text-sm">
              <thead className="border-b border-white/10 font-mono text-[11px] uppercase text-slate-500"><tr><th className="px-4 py-3">Started</th><th className="px-4 py-3">Status</th><th className="px-4 py-3">Latest attempt duration</th><th className="px-4 py-3">Trigger</th><th className="px-4 py-3">Revision</th></tr></thead>
              <tbody className="divide-y divide-white/[0.07]">{detail.recent_executions.map((execution) => <RecentExecutionRow key={execution.execution_id} actionId={detail.action_id} execution={execution} />)}</tbody>
            </table>
          </div>
        )}
      </Panel>
    </div>
  );
}

function ConfigurationTab({ detail, effectivePolicy }: { detail: ActionDetailDto; effectivePolicy?: EffectiveActionPolicy }) {
  const schedule = useSchedule(scheduleStableKey(detail.definition));
  const unavailable = "Not configured by the current Ryvus model";

  return <div className="grid gap-4">
    <ConfigurationSection title="General" message="Current read-only runtime and execution policy.">
      <dl className="grid gap-x-6 gap-y-4 sm:grid-cols-2 xl:grid-cols-3">
        <Meta label="Runtime" value={detail.definition.runtime} />
        <Meta label="Timeout" value={effectivePolicy?.timeout ?? "Unavailable"} />
        <Meta label="Max attempts" value={effectivePolicy ? String(effectivePolicy.retry.max_attempts) : "Unavailable"} />
        <Meta label="Initial delay" value={effectivePolicy?.retry.initial_delay ?? "Unavailable"} />
        <Meta label="Backoff" value={effectivePolicy ? String(effectivePolicy.retry.backoff) : "Unavailable"} />
        <Meta label="Memory" value={unavailable} />
        <Meta label="Temporary storage" value={unavailable} />
        <Meta label="Maximum concurrency" value={unavailable} />
      </dl>
    </ConfigurationSection>
    <ConfigurationSection title="Environment Variables" message="Action-scoped environment configuration is not available."><UnavailablePlaceholder message="Ryvus does not inspect project or process environment values here." /></ConfigurationSection>
    <ConfigurationSection title="Secrets" message="Action-scoped secret management is not available."><UnavailablePlaceholder message="No inferred local secret material is displayed." /></ConfigurationSection>
    <ConfigurationSection title="Capabilities" message="Declared capabilities describe portable action intent; resolved bindings belong to the platform. Neither is configurable or resolved in the current model." />
    <ConfigurationSection title="Triggers" message="Current trigger metadata is read-only."><TriggerDetails definition={detail.definition} schedule={schedule} /></ConfigurationSection>
  </div>;
}

function RevisionsTab({ detail, selectedRevision }: { detail: ActionDetailDto; selectedRevision: string | null }) {
  const revisions = useQuery({
    queryKey: ["action-revisions", detail.action_id],
    queryFn: () => actionsApi.revisions(detail.action_id),
  });

  if (revisions.isError) return <EmptyState title="Observed revisions unavailable" message={errorMessage(revisions.error)} />;
  if (!revisions.data) return <EmptyState title="Loading observed revisions" message="Reading bounded execution and Runtime Host stream history." />;
  const selected = selectedRevision ? revisions.data.revisions.find((revision) => revision.revision === selectedRevision) : undefined;

  return <div className="grid gap-4">
    {(revisions.data.execution_history_truncated || revisions.data.log_history_truncated) && <Panel className="border-amber-300/20 bg-amber-400/[0.04] p-4"><p className="text-sm text-amber-100">Observed revision history is bounded and incomplete for {revisions.data.execution_history_truncated && revisions.data.log_history_truncated ? "execution and log" : revisions.data.execution_history_truncated ? "execution" : "log"} history.</p></Panel>}
    <Panel className="p-4 sm:p-5"><SectionHeading title="Observed revisions" message="Composed from the current catalog plus bounded execution and Runtime Host stream history. This is not discovery history." /></Panel>
    {selectedRevision && !selected && <Panel className="border-amber-300/20 bg-amber-400/[0.04] p-4"><h2 className="text-sm font-semibold text-amber-100">Revision not observed</h2><p className="mt-1 text-xs leading-5 text-amber-200/70">This revision is not present in the retained observed history.</p></Panel>}
    {selected ? <RevisionDetail action={detail} revision={selected} /> : revisions.data.revisions.length === 0 ? <EmptyState title="No observed revisions" message="No current or historical revision metadata is available." /> : (
      <Panel className="overflow-x-auto"><table className="w-full min-w-[920px] border-collapse text-left text-sm"><thead className="border-b border-white/10 font-mono text-[11px] uppercase text-slate-500"><tr><th className="px-4 py-3">Revision</th><th className="px-4 py-3">Status</th><th className="px-4 py-3">First observed</th><th className="px-4 py-3">Last observed</th><th className="px-4 py-3">Runtime</th><th className="px-4 py-3 text-right">Executions</th><th className="px-4 py-3 text-right">Streams</th></tr></thead><tbody className="divide-y divide-white/[0.07]">{revisions.data.revisions.map((revision) => <RevisionRow key={revision.revision} actionId={detail.action_id} revision={revision} />)}</tbody></table></Panel>
    )}
  </div>;
}

function RevisionDetail({ action, revision }: { action: ActionDetailDto; revision: ObservedRevision }) {
  const current = revision.revision === action.current_revision;
  const linkClass = "inline-flex min-h-10 items-center rounded-md border border-cyan-300/20 px-3 font-mono text-xs font-bold text-cyan-200 hover:border-cyan-300/40 hover:bg-cyan-400/[0.06] hover:text-white focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300";
  return <div className="grid gap-4">
    <Panel className="grid gap-4 p-4 sm:p-5">
      <div className="flex flex-wrap items-start justify-between gap-3"><div className="min-w-0"><p className="font-mono text-[11px] font-bold uppercase text-slate-500">Revision detail</p><h2 className="mt-1 break-all font-mono text-sm text-white">{revision.revision}</h2></div><Badge tone={current ? "green" : "slate"}>{current ? "Current" : "Observed"}</Badge></div>
      <dl className="grid gap-x-6 gap-y-4 sm:grid-cols-2 lg:grid-cols-4"><Meta label="First observed" value={formatUnixNanos(revision.first_observed_at_unix_nanos)} /><Meta label="Last observed" value={formatUnixNanos(revision.last_observed_at_unix_nanos)} /><Meta label="Runtime" value={revision.runtime ?? "Unavailable"} /><Meta label="Executions" value={String(revision.execution_count)} /><Meta label="Runtime Host streams" value={String(revision.runtime_host_stream_count)} /></dl>
      <div className="flex flex-wrap gap-2"><a className={linkClass} href={actionHref(action.action_id, "executions", { revision: revision.revision })}>View exact-revision executions</a><a className={linkClass} href={actionHref(action.action_id, "logs", { revision: revision.revision })}>View exact-revision logs</a><a className={linkClass} href={actionHref(action.action_id, "revisions")}>Back to revisions</a></div>
    </Panel>
    {current ? <Panel className="p-4 sm:p-5"><SectionHeading title="Current definition" message="Retained from the current action catalog." /><dl className="mt-4 grid gap-x-6 gap-y-4 sm:grid-cols-2"><Meta label="Action key" value={action.action_id} mono /><Meta label="Kind" value={actionKindLabel(action.definition)} /><Meta label="Source" value={action.definition.source} mono /><Meta label="Entrypoint" value={action.definition.entrypoint} mono /></dl></Panel> : <EmptyState title="Definition metadata not retained" message="Ryvus observed this revision in execution or log history, but does not retain its discovery-time definition." />}
  </div>;
}

function RecentExecutionRow({ actionId: selectedActionId, execution }: { actionId: string; execution: ExecutionAggregate }) {
  return <tr className="transition-colors hover:bg-white/[0.035]"><td className="p-0"><a className="block min-h-11 px-4 py-3 text-slate-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-violet-300" href={actionHref(selectedActionId, "executions", { execution_id: execution.execution_id, revision: execution.action_revision })}>{formatTimestamp(execution.created_at)}</a></td><td className="px-4 py-3"><Badge tone={stateTone(execution.state)}>{execution.state}</Badge></td><td className="px-4 py-3 font-mono text-xs tabular-nums text-slate-400">{latestDuration(execution)}</td><td className="px-4 py-3 text-slate-300">{execution.trigger.type}</td><td className="max-w-64 truncate px-4 py-3 font-mono text-xs text-slate-500" title={execution.action_revision}>{execution.action_revision}</td></tr>;
}

function RevisionRow({ actionId: selectedActionId, revision }: { actionId: string; revision: ObservedRevision }) {
  return <tr className="transition-colors hover:bg-white/[0.035]"><td className="p-0"><a href={actionHref(selectedActionId, "revisions", { revision: revision.revision })} className="block min-h-11 max-w-80 truncate px-4 py-3 font-mono text-xs text-cyan-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-violet-300" title={revision.revision}>{revision.revision}</a></td><td className="px-4 py-3"><Badge tone={revision.status === "current" ? "green" : "slate"}>{revision.status === "current" ? "Current" : "Observed"}</Badge></td><td className="px-4 py-3 text-slate-400">{formatUnixNanos(revision.first_observed_at_unix_nanos)}</td><td className="px-4 py-3 text-slate-400">{formatUnixNanos(revision.last_observed_at_unix_nanos)}</td><td className="px-4 py-3 text-slate-300">{revision.runtime ?? "Unavailable"}</td><td className="px-4 py-3 text-right font-mono tabular-nums text-slate-300">{revision.execution_count}</td><td className="px-4 py-3 text-right font-mono tabular-nums text-slate-300">{revision.runtime_host_stream_count}</td></tr>;
}

function ConfigurationSection({ title, message, children }: { title: string; message: string; children?: React.ReactNode }) {
  return <Panel className="p-4 sm:p-5"><SectionHeading title={title} message={message} />{children && <div className="mt-4">{children}</div>}</Panel>;
}

function UnavailablePlaceholder({ message }: { message: string }) {
  return <div className="flex gap-3 rounded-md border border-white/[0.08] bg-black/20 p-3"><span className="mt-1 h-2 w-2 shrink-0 rounded-sm bg-amber-300/70" /><div><p className="text-sm font-semibold text-slate-300">Unavailable</p><p className="mt-1 text-xs leading-5 text-slate-500">{message}</p></div></div>;
}

function SectionHeading({ title, message }: { title: string; message: string }) {
  return <div><h2 className="text-base font-semibold text-white">{title}</h2><p className="mt-1 text-xs leading-5 text-slate-500">{message}</p></div>;
}

function HealthMetric({ label, value, tone }: { label: string; value: string; tone?: string }) {
  return <div className="min-w-0 p-4 sm:p-5"><dt className="font-mono text-[10px] font-bold uppercase leading-4 text-slate-600">{label}</dt><dd className={cn("mt-2 font-mono text-xl font-semibold tabular-nums text-white", tone)}>{value}</dd></div>;
}

function TriggerDetails({ definition, schedule }: { definition: ActionDefinition; schedule: ReturnType<typeof useSchedule> }) {
  const trigger = triggerMetadata(definition);
  const stableKey = scheduleStableKey(definition);
  const pending = schedule.isPending;
  const unavailable = schedule.isError;
  const notReconciled = !pending && !unavailable && !schedule.data;
  return <dl className="mt-4 grid gap-4 sm:grid-cols-2">
    {trigger.map(([label, value]) => <Meta key={label} label={label} value={value} mono={label === "Route" || label === "Schedule ID"} />)}
    {stableKey && <Meta label="Schedule ID" value={pending ? "Loading" : unavailable ? "Unavailable" : notReconciled ? "Not reconciled" : schedule.data?.schedule_id ?? "Unavailable"} mono />}
    {stableKey && <Meta label="Availability" value={pending ? "Loading" : unavailable ? "Unavailable" : notReconciled ? "Not reconciled" : schedule.data?.availability ?? "Unavailable"} />}
    {stableKey && <Meta label="Enablement" value={pending ? "Loading" : unavailable ? "Unavailable" : notReconciled ? "Unavailable · not reconciled" : schedule.data?.enablement ?? "Unavailable"} />}
    {stableKey && <Meta label="Next trigger" value={pending ? "Loading" : unavailable ? "Unavailable" : notReconciled ? "Unavailable · not reconciled" : formatTimestamp(schedule.data?.next_trigger_at)} />}
  </dl>;
}

function useSchedule(stableKey?: string) {
  return useQuery({ queryKey: ["schedule-by-stable-key", stableKey], queryFn: () => loadScheduleByStableKey(stableKey!), enabled: Boolean(stableKey), retry: false });
}

async function loadScheduleByStableKey(stableKey: string) {
  let cursor: string | undefined;
  do {
    const page = await historyApi.schedules(cursor, 100);
    const schedule = page.items.find((item) => item.stable_schedule_key === stableKey);
    if (schedule) return schedule;
    cursor = page.next_cursor ?? undefined;
  } while (cursor);
  return null;
}

function scheduleStableKey(definition: ActionDefinition) {
  const kind: Record<string, unknown> = definition.kind;
  return isRecord(kind.Schedule) && typeof kind.Schedule.key === "string" && kind.Schedule.key ? kind.Schedule.key : undefined;
}

function triggerMetadata(definition: ActionDefinition): Array<[string, string]> {
  const kind: Record<string, unknown> = definition.kind;
  if (isRecord(kind.Api)) return [["Kind", "API"], ["Method", stringValue(kind.Api.method)], ["Route", stringValue(kind.Api.path)]];
  if (isRecord(kind.Schedule)) return [["Kind", "Schedule"], ["Expression", stringValue(kind.Schedule.expression)], ["Schedule key", stringValue(kind.Schedule.key)]];
  if (isRecord(kind.Queue)) return [["Kind", "Queue"], ["Queue", stringValue(kind.Queue.queue)]];
  if ("Flow" in definition.kind) return [["Kind", "Flow"], ["Entrypoint", definition.entrypoint]];
  if ("Authorizer" in definition.kind) return [["Kind", "Authorizer"], ["Entrypoint", definition.entrypoint]];
  return [["Kind", "Unavailable"]];
}

function isRecord(value: unknown): value is Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value); }
function stringValue(value: unknown) { return typeof value === "string" && value ? value : "Unavailable"; }

function actionHref(actionId: string, tab: ActionTab, context: Record<string, string | null | undefined> = {}) {
  const query = new URLSearchParams({ action_id: actionId, tab });
  for (const [key, value] of Object.entries(context)) if (value) query.set(key, value);
  return `#actions?${query}`;
}

function actionTabHref(actionId: string, tab: ActionTab, current: URLSearchParams) {
  const context: Record<string, string | null> = { revision: current.get("revision") };
  if (tab === "executions" || tab === "logs") context.execution_id = current.get("execution_id");
  if (tab === "logs") {
    context.attempt_id = current.get("attempt_id");
    context.runtime_host_id = current.get("runtime_host_id");
  }
  return actionHref(actionId, tab, context);
}

function formatPercent(value: number | null | undefined) { return value == null ? "—" : `${Math.round(value * 100)}%`; }
function formatMilliseconds(value: number | null | undefined) { return value == null ? "—" : `${Math.round(value)}ms`; }
function latestDuration(execution: ExecutionAggregate) {
  for (let index = execution.attempts.length - 1; index >= 0; index -= 1) {
    const duration = execution.attempts[index].result?.duration;
    if (duration) return formatMilliseconds(duration.secs * 1_000 + duration.nanos / 1_000_000);
  }
  return "—";
}

function formatUnixNanos(value: string | null | undefined) {
  if (!value) return "Unavailable";
  try { return formatDate(new Date(Number(BigInt(value) / 1_000_000n))); } catch { return "Unavailable"; }
}

function formatTimestamp(value: unknown) {
  if (value == null) return "Unavailable";
  if (typeof value === "string") return formatDate(new Date(value));
  if (typeof value !== "object") return "Unavailable";
  const timestamp = value as Record<string, unknown>;
  if (typeof timestamp.secs_since_epoch !== "number" || typeof timestamp.nanos_since_epoch !== "number") return "Unavailable";
  return formatDate(new Date(timestamp.secs_since_epoch * 1_000 + timestamp.nanos_since_epoch / 1_000_000));
}

function formatDate(date: Date) {
  return Number.isNaN(date.getTime()) ? "Unavailable" : date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" });
}

function stateTone(state: string): "green" | "red" | "amber" | "blue" | "slate" {
  return state === "succeeded" ? "green" : ["failed", "cancelled", "timed_out"].includes(state) ? "red" : state === "cancellation_requested" ? "amber" : state === "running" ? "blue" : "slate";
}

function Meta({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return <div className="min-w-0"><dt className="font-mono text-[10px] font-bold uppercase text-slate-600">{label}</dt><dd className={cn("mt-1 truncate text-slate-300", mono && "max-w-52 font-mono text-xs")} title={value}>{value}</dd></div>;
}

function actionId(action: ActionDefinition) {
  return action.name ?? action.entrypoint;
}

function actionKindLabel(action: ActionDefinition) {
  if ("Api" in action.kind) return "API";
  if ("Schedule" in action.kind) return "Schedule";
  if ("Flow" in action.kind) return "Flow";
  if ("Authorizer" in action.kind) return "Authorizer";
  if ("Queue" in action.kind) return "Queue";
  return "Unknown";
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : "Request failed";
}
