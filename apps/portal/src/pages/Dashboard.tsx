import type { ApiActionKind, Artifacts } from "../artifacts/types";
import { isApiAction } from "../artifacts/types";
import { Badge, Page, Panel } from "../components/ui";

export function Dashboard({ artifacts }: { artifacts: Artifacts }) {
  const apiActions = artifacts.catalog.actions.filter(isApiAction);
  const runtimes = runtimeCounts(artifacts);
  const methods = methodCounts(apiActions);
  const enabledSchedules = artifacts.schedules.schedules.filter((schedule) => schedule.enabled).length;
  const disabledSchedules = artifacts.schedules.schedules.length - enabledSchedules;
  const flowSteps = artifacts.flows.flows.reduce((count, flow) => count + flow.steps.length, 0);
  const docsPages = artifacts.docsRegistry.pages.length;

  return (
    <Page eyebrow="Project" title="Dashboard">
      <div className="grid gap-4">
        <div className="grid gap-3 sm:grid-cols-2 2xl:grid-cols-5">
          <Summary label="Gateway routes" value={apiActions.length.toString()} meta={`${methods.length} methods`} />
          <Summary label="Schedules" value={artifacts.schedules.schedules.length.toString()} meta={`${enabledSchedules} enabled`} />
          <Summary label="Flows" value={artifacts.flows.flows.length.toString()} meta={`${flowSteps} steps`} />
          <Summary label="Runtimes" value={runtimes.length.toString()} meta={runtimes.map((runtime) => runtime.name).join(", ") || "none"} />
          <Summary label="Docs" value={docsPages.toString()} meta="registry loaded" />
        </div>

        <div className="grid gap-4 xl:grid-cols-[minmax(0,1.15fr)_minmax(340px,0.85fr)]">
          <Panel className="grid gap-4 p-4">
            <SectionHeader title="Runtime coverage" meta="Discovered executable surface" />
            <div className="grid gap-3">
              {runtimes.length === 0 ? (
                <p className="text-sm text-slate-400">No runtimes discovered.</p>
              ) : (
                runtimes.map((runtime) => (
                  <Meter key={runtime.name} label={runtime.name} value={runtime.count} max={artifacts.catalog.actions.length} />
                ))
              )}
            </div>
          </Panel>

          <Panel className="grid gap-4 p-4">
            <SectionHeader title="Schedule readiness" meta="Local scheduler inputs" />
            <div className="grid grid-cols-2 gap-3">
              <StatusTile label="Enabled" value={enabledSchedules} tone="green" />
              <StatusTile label="Disabled" value={disabledSchedules} tone={disabledSchedules ? "red" : "slate"} />
            </div>
            <div className="grid gap-2">
              {artifacts.schedules.schedules.slice(0, 4).map((schedule) => (
                <div key={schedule.name} className="flex min-w-0 items-center justify-between gap-3 rounded-lg border border-white/10 bg-black/20 px-3 py-2">
                  <span className="min-w-0">
                    <strong className="block truncate text-sm text-slate-100">{schedule.name}</strong>
                    <code className="block truncate text-xs text-slate-500">{schedule.expression}</code>
                  </span>
                  <Badge tone={schedule.enabled ? "green" : "slate"}>{schedule.enabled ? "enabled" : "off"}</Badge>
                </div>
              ))}
            </div>
          </Panel>
        </div>

        <div className="grid gap-4 xl:grid-cols-3">
          <Panel className="grid gap-4 p-4">
            <SectionHeader title="API method mix" meta="OpenAPI routes" />
            <div className="grid gap-2">
              {methods.length === 0 ? (
                <p className="text-sm text-slate-400">No API methods discovered.</p>
              ) : (
                methods.map((method) => (
                  <Meter key={method.name} label={method.name} value={method.count} max={apiActions.length} />
                ))
              )}
            </div>
          </Panel>

          <Panel className="grid gap-4 p-4">
            <SectionHeader title="Flow readiness" meta="Declarative workflow surface" />
            <div className="grid gap-2">
              {artifacts.flows.flows.length === 0 ? (
                <p className="text-sm text-slate-400">No flows discovered.</p>
              ) : (
                artifacts.flows.flows.map((flow) => (
                  <div key={flow.key} className="rounded-lg border border-white/10 bg-black/20 p-3">
                    <div className="flex items-center justify-between gap-3">
                      <strong className="truncate text-sm text-white">{flow.key}</strong>
                      <Badge tone="violet">{flow.steps.length} steps</Badge>
                    </div>
                    {flow.description && <p className="mt-2 max-h-10 overflow-hidden text-xs leading-5 text-slate-500">{flow.description}</p>}
                  </div>
                ))
              )}
            </div>
          </Panel>

          <Panel className="grid gap-4 p-4">
            <SectionHeader title="Control artifacts" meta="Portal data sources" />
            <Checklist
              items={[
                ["Catalog", artifacts.catalog.actions.length > 0],
                ["OpenAPI", Object.keys(artifacts.openapi.paths).length > 0],
                ["Schedules", artifacts.schedules.schedules.length > 0],
                ["Flows", artifacts.flows.flows.length > 0],
                ["Docs registry", docsPages > 0],
              ]}
            />
          </Panel>
        </div>
      </div>
    </Page>
  );
}

function Summary({ label, value, meta }: { label: string; value: string; meta: string }) {
  return (
    <Panel className="p-4">
      <div className="text-xs font-medium text-slate-400">{label}</div>
      <div className="mt-2 truncate text-2xl font-semibold tracking-tight text-white">{value}</div>
      <div className="mt-1 truncate text-xs text-slate-500">{meta}</div>
    </Panel>
  );
}

function SectionHeader({ title, meta }: { title: string; meta: string }) {
  return (
    <div>
      <h2 className="text-sm font-semibold text-white">{title}</h2>
      <p className="mt-1 text-xs text-slate-500">{meta}</p>
    </div>
  );
}

function Meter({ label, value, max }: { label: string; value: number; max: number }) {
  const percent = max ? Math.round((value / max) * 100) : 0;

  return (
    <div className="grid gap-2">
      <div className="flex items-center justify-between gap-3 text-sm">
        <span className="font-medium text-slate-200">{label}</span>
        <span className="text-slate-500">{value}</span>
      </div>
      <div className="h-2 overflow-hidden rounded-full bg-white/8">
        <div className="h-full rounded-full bg-blue-500" style={{ width: `${percent}%` }} />
      </div>
    </div>
  );
}

function StatusTile({ label, value, tone }: { label: string; value: number; tone: "green" | "red" | "slate" }) {
  return (
    <div className="rounded-lg border border-white/10 bg-black/20 p-3">
      <div className="text-xs text-slate-500">{label}</div>
      <div className="mt-1 flex items-center justify-between gap-2">
        <strong className="text-xl text-white">{value}</strong>
        <Badge tone={tone}>{tone === "green" ? "ready" : tone === "red" ? "check" : "none"}</Badge>
      </div>
    </div>
  );
}

function Checklist({ items }: { items: Array<[string, boolean]> }) {
  return (
    <div className="grid gap-2">
      {items.map(([label, ok]) => (
        <div key={label} className="flex items-center justify-between gap-3 rounded-lg border border-white/10 bg-black/20 px-3 py-2">
          <span className="text-sm text-slate-300">{label}</span>
          <Badge tone={ok ? "green" : "slate"}>{ok ? "loaded" : "empty"}</Badge>
        </div>
      ))}
    </div>
  );
}

function runtimeCounts(artifacts: Artifacts) {
  return counted(artifacts.catalog.actions.map((action) => action.runtime));
}

function methodCounts(actions: Array<{ kind: ApiActionKind }>) {
  return counted(actions.map((action) => action.kind.Api.method.toUpperCase()));
}

function counted(values: string[]) {
  const counts = new Map<string, number>();
  for (const value of values) {
    counts.set(value, (counts.get(value) ?? 0) + 1);
  }
  return Array.from(counts, ([name, count]) => ({ name, count })).sort((left, right) => right.count - left.count || left.name.localeCompare(right.name));
}
