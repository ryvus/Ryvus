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

const Flows = lazy(() => import("../pages/Flows").then((module) => ({ default: module.Flows })));

const routes = [
  ["dashboard", "Dashboard"],
  ["gateway", "Gateway"],
  ["schedules", "Schedules"],
  ["flows", "Flows"],
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

  return (
    <main className="min-h-screen bg-slate-950 text-slate-100">
      <div className="grid min-h-screen grid-cols-1 lg:grid-cols-[260px_minmax(0,1fr)]">
        <aside className="border-b border-white/10 bg-[#07111f] px-4 py-4 lg:border-b-0 lg:border-r lg:px-3">
          <div className="mb-5 flex items-center gap-3 px-2">
            <img
              src={ryvusMark}
              alt=""
              className="h-9 w-9 rounded-xl bg-slate-950 shadow-lg shadow-blue-950/50"
            />
            <span className="grid leading-tight">
              <strong className="text-sm font-semibold text-white">Ryvus</strong>
              <small className="text-xs font-medium text-slate-400">Portal</small>
            </span>
          </div>
          <nav className="grid gap-1" aria-label="Portal sections">
            {routes.map(([id, label]) => (
              <a
                key={id}
                href={`#${id}`}
                className={cn(
                  "rounded-lg border border-transparent px-3 py-2 text-sm font-medium text-slate-400 transition hover:bg-white/[0.06] hover:text-white focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-300",
                  route === id &&
                    "border-blue-400/20 bg-blue-500/10 text-white shadow-[inset_2px_0_0_#2563ff]",
                )}
              >
                {label}
              </a>
            ))}
          </nav>
        </aside>

        <div className="min-w-0 bg-[radial-gradient(circle_at_top_right,rgba(37,99,255,0.18),transparent_34rem),linear-gradient(180deg,#0b1220_0%,#020617_100%)]">
          <header className="sticky top-0 z-20 flex min-h-16 items-center justify-between border-b border-white/10 bg-slate-950/75 px-5 backdrop-blur-xl sm:px-8">
            <div className="grid gap-0.5">
              <span className="text-[11px] font-semibold uppercase text-blue-400">Local Snapshot</span>
              <strong className="text-sm font-semibold text-white">
                {artifacts.data?.openapi.info?.title ?? "Ryvus Public API"}
              </strong>
            </div>
            <Badge tone={artifacts.isError ? "red" : artifacts.data ? "blue" : "slate"}>
              {status}
            </Badge>
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
  const hash = window.location.hash.replace("#", "");
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
    case "sdk-docs":
      return (
        <EmptyState
          title="SDK Docs"
          message="SDK docs are not included in this artifact snapshot."
        />
      );
    case "execution-preview":
      return (
        <EmptyState
          title="Execution Preview"
          message="Execution preview will call runtime APIs in a later slice."
        />
      );
  }
}
