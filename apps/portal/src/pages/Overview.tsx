import type { Artifacts } from "../artifacts/types";
import { isApiAction } from "../artifacts/types";

export function Overview({ artifacts }: { artifacts: Artifacts }) {
  const apiCount = artifacts.catalog.actions.filter(isApiAction).length;
  const runtimes = Array.from(
    new Set(artifacts.catalog.actions.map((action) => action.runtime)),
  ).sort();

  return (
    <div className="page">
      <h1>Overview</h1>
      <div className="summary-grid">
        <Summary label="Artifact Snapshot" value="Loaded" />
        <Summary label="API Actions" value={apiCount.toString()} />
        <Summary label="Schedules" value={artifacts.schedules.schedules.length.toString()} />
        <Summary label="Runtime Types" value={runtimes.join(", ") || "None"} />
        <Summary label="Docs Pages" value={artifacts.docsRegistry.pages.length.toString()} />
      </div>
    </div>
  );
}

function Summary({ label, value }: { label: string; value: string }) {
  return (
    <div className="summary-item">
      <div className="summary-label">{label}</div>
      <div className="summary-value">{value}</div>
    </div>
  );
}
