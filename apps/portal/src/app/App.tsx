import { useEffect, useState } from "react";
import { loadArtifacts } from "../artifacts/load";
import type { Artifacts } from "../artifacts/types";
import { ApiActions } from "../pages/ApiActions";
import { Docs } from "../pages/Docs";
import { EmptyState } from "../pages/EmptyState";
import { Overview } from "../pages/Overview";
import { Schedules } from "../pages/Schedules";

const routes = [
  ["overview", "Overview"],
  ["api-actions", "API Actions"],
  ["schedules", "Schedules"],
  ["docs", "Docs"],
  ["sdk-docs", "SDK Docs"],
  ["execution-preview", "Execution Preview"],
] as const;

type RouteId = (typeof routes)[number][0];

export function App() {
  const [route, setRoute] = useState<RouteId>(currentRoute());
  const [artifacts, setArtifacts] = useState<Artifacts | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    const onHashChange = () => setRoute(currentRoute());
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  useEffect(() => {
    loadArtifacts()
      .then((value) => {
        setArtifacts(value);
        setError("");
      })
      .catch((err: unknown) => {
        setError(err instanceof Error ? err.message : "failed to load artifacts");
      });
  }, []);

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark">R</span>
          <span>Ryvus Portal</span>
        </div>
        <nav aria-label="Portal sections">
          {routes.map(([id, label]) => (
            <a key={id} href={`#${id}`} className={route === id ? "active" : ""}>
              {label}
            </a>
          ))}
        </nav>
      </aside>
      <div className="workspace">
        <header className="topbar">
          <div>
            <span className="eyebrow">Local snapshot</span>
            <strong>{artifacts?.openapi.info?.title ?? "Ryvus Public API"}</strong>
          </div>
          <span className={error ? "status-pill error-pill" : "status-pill"}>
            {error ? "Artifact error" : artifacts ? "Artifacts loaded" : "Loading"}
          </span>
        </header>
        <section className="content">
          {error ? (
            <ArtifactError message={error} />
          ) : artifacts ? (
            renderRoute(route, artifacts)
          ) : (
            <p>Loading artifacts...</p>
          )}
        </section>
      </div>
    </main>
  );
}

function currentRoute(): RouteId {
  const hash = window.location.hash.replace("#", "");
  return routes.some(([id]) => id === hash) ? (hash as RouteId) : "overview";
}

function renderRoute(route: RouteId, artifacts: Artifacts) {
  switch (route) {
    case "overview":
      return <Overview artifacts={artifacts} />;
    case "api-actions":
      return <ApiActions artifacts={artifacts} />;
    case "schedules":
      return <Schedules artifacts={artifacts} />;
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
          message="Execution preview will call gateway runtime APIs in a later slice."
        />
      );
  }
}

function ArtifactError({ message }: { message: string }) {
  return (
    <div className="page error-state">
      <h1>Artifact Error</h1>
      <p>{message}</p>
    </div>
  );
}
