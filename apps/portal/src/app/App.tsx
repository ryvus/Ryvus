import { useQuery } from "@tanstack/react-query";
import { lazy, Suspense, useEffect, useState } from "react";
import ryvusMark from "../assets/ryvus-mark.svg";
import { loadArtifacts } from "../artifacts/load";
import type { Artifacts } from "../artifacts/types";
import { Badge, EmptyState, cn } from "../components/ui";
import { ApiActions } from "../pages/ApiActions";
import { Docs } from "../pages/Docs";
import { Dashboard } from "../pages/Dashboard";
import { Schedules } from "../pages/Schedules";
import { Executions } from "../pages/Executions";
import { Logs } from "../pages/Logs";

const Flows = lazy(() => import("../pages/Flows").then((module) => ({ default: module.Flows })));

const routes = [
  ["dashboard", "Dashboard"],
  ["gateway", "Gateway"],
  ["schedules", "Schedules"],
  ["flows", "Flows"],
  ["logs", "Logs"],
  ["docs", "Docs"],
  ["sdk-docs", "SDK Docs"],
  ["execution-preview", "Execution Preview"],
] as const;

type RouteId = (typeof routes)[number][0];

export function App() {
  const [route, setRoute] = useState<RouteId>(currentRoute());
  const artifacts = useQuery({
    queryKey: ["artifacts"],
    queryFn: loadArtifacts,
  });

  useEffect(() => {
    const onHashChange = () => setRoute(currentRoute());
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  const status = artifacts.isError
    ? "Artifact error"
    : artifacts.data
      ? "Artifacts loaded"
      : "Loading";
  const routeLabel = routes.find(([id]) => id === route)?.[1] ?? "Dashboard";

  return (
    <main className="min-h-screen overflow-x-hidden bg-[#08090b] text-slate-100">
      <div className="grid min-h-screen grid-cols-1 lg:grid-cols-[248px_minmax(0,1fr)]">
        <aside className="border-b border-white/10 bg-[#090a0c] px-3 py-4 lg:border-b-0 lg:border-r">
          <div className="mb-8 flex items-center gap-3 px-2">
            <img
              src={ryvusMark}
              alt=""
              className="h-8 w-8 rounded-md bg-[#050506] ring-1 ring-white/10"
            />
            <span className="grid leading-tight">
              <strong className="text-sm font-semibold tracking-tight text-white">.ryvus</strong>
              <small className="font-mono text-[11px] font-medium uppercase text-slate-600">Control</small>
            </span>
          </div>
          <nav className="grid gap-1" aria-label="Portal sections">
            {routes.map(([id, label]) => (
              <a
                key={id}
                href={`#${id}`}
                className={cn(
                  "rounded-md border border-transparent px-3 py-2 text-sm font-semibold text-slate-500 transition hover:bg-white/[0.045] hover:text-slate-100 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-300",
                  route === id &&
                    "border-white/10 bg-[#17181c] text-white shadow-[inset_2px_0_0_#6f3dff]",
                )}
              >
                {label}
              </a>
            ))}
          </nav>
        </aside>

        <div className="min-w-0 bg-[#0d0e11]">
          <header className="sticky top-0 z-20 flex min-h-14 items-center justify-between gap-4 border-b border-white/10 bg-[#0d0e11]/95 px-5 backdrop-blur sm:px-8">
            <div className="flex min-w-0 items-center gap-2 text-sm">
              <span className="truncate font-semibold text-slate-500">Portal</span>
              <span className="text-slate-700">/</span>
              <span className="truncate font-semibold text-slate-400">{routeLabel}</span>
              <span className="text-slate-700">/</span>
              <strong className="truncate font-semibold text-white">
                {artifacts.data?.openapi.info?.title ?? "Ryvus Public API"}
              </strong>
            </div>
            <span className="hidden shrink-0 sm:block">
              <Badge tone={artifacts.isError ? "red" : artifacts.data ? "blue" : "slate"}>
                {status}
              </Badge>
            </span>
          </header>

          <section className="min-w-0 px-5 py-6 sm:px-8 lg:py-8">
            {artifacts.isError ? (
              <EmptyState
                title="Artifact Error"
                message={artifacts.error instanceof Error ? artifacts.error.message : "Failed to load artifacts."}
              />
            ) : artifacts.data ? (
              renderRoute(route, artifacts.data)
            ) : (
              <EmptyState title="Loading artifacts" message="Reading generated Ryvus artifacts from Control." />
            )}
          </section>
        </div>
      </div>
    </main>
  );
}

function currentRoute(): RouteId {
  const hash = window.location.hash.replace("#", "").split("?")[0];
  if (hash === "overview") {
    return "dashboard";
  }
  if (hash === "api-actions") {
    return "gateway";
  }
  return routes.some(([id]) => id === hash) ? (hash as RouteId) : "dashboard";
}

function renderRoute(route: RouteId, artifacts: Artifacts) {
  switch (route) {
    case "dashboard":
      return <Dashboard artifacts={artifacts} />;
    case "gateway":
      return <ApiActions artifacts={artifacts} />;
    case "schedules":
      return <Schedules artifacts={artifacts} />;
    case "flows":
      return (
        <Suspense fallback={<EmptyState title="Loading flows" message="Preparing the graph renderer." />}>
          <Flows artifacts={artifacts} />
        </Suspense>
      );
    case "docs":
      return <Docs artifacts={artifacts} />;
    case "logs":
      return <Logs />;
    case "sdk-docs":
      return (
        <EmptyState
          title="SDK Docs"
          message="SDK docs are not included in this artifact snapshot."
        />
      );
    case "execution-preview":
      return <Executions />;
  }
}
