import type { Artifacts, DocsRegistryPage } from "./types";

async function loadJson<T>(path: string): Promise<T> {
  const response = await fetch(path);
  if (!response.ok) {
    throw new Error(`${path} returned ${response.status}`);
  }
  return (await response.json()) as T;
}

async function loadText(path: string): Promise<string> {
  const response = await fetch(path);
  if (!response.ok) {
    throw new Error(`${path} returned ${response.status}`);
  }
  return response.text();
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isString(value: unknown): value is string {
  return typeof value === "string";
}

function validateActionKind(kind: Record<string, unknown>): void {
  if ("Api" in kind) {
    if (
      !isRecord(kind.Api) ||
      !isString(kind.Api.method) ||
      !isString(kind.Api.path)
    ) {
      throw new Error(
        "Invalid catalog artifact: Api actions require { method: string, path: string }",
      );
    }
  }

  if ("Schedule" in kind) {
    if (!isRecord(kind.Schedule) || !isString(kind.Schedule.expression)) {
      throw new Error(
        "Invalid catalog artifact: Schedule actions require { expression: string }",
      );
    }
  }
}

function validateCatalog(value: unknown): asserts value is Artifacts["catalog"] {
  if (
    !isRecord(value) ||
    !Array.isArray(value.actions) ||
    value.actions.some(
      (action) =>
        !isRecord(action) ||
        !isString(action.runtime) ||
        !isString(action.source) ||
        !isString(action.entrypoint) ||
        !isRecord(action.kind),
    )
  ) {
    throw new Error("Invalid catalog artifact: expected { actions: [] }");
  }

  for (const action of value.actions) {
    validateActionKind(action.kind);
  }
}

function validateOpenApi(value: unknown): asserts value is Artifacts["openapi"] {
  if (
    !isRecord(value) ||
    typeof value.openapi !== "string" ||
    !isRecord(value.paths)
  ) {
    throw new Error("Invalid openapi artifact: expected an OpenAPI document");
  }
}

function validateSchedules(value: unknown): asserts value is Artifacts["schedules"] {
  if (
    !isRecord(value) ||
    !Array.isArray(value.schedules) ||
    value.schedules.some((schedule) => !isRecord(schedule))
  ) {
    throw new Error("Invalid schedules artifact: expected { schedules: [] }");
  }
}

function validateDocsRegistry(value: unknown): asserts value is Artifacts["docsRegistry"] {
  if (
    !isRecord(value) ||
    !Array.isArray(value.nav) ||
    !Array.isArray(value.pages) ||
    value.nav.some((entry) => !isRecord(entry)) ||
    value.pages.some((page) => !isRecord(page))
  ) {
    throw new Error("Invalid docs registry artifact: expected { nav: [], pages: [] }");
  }
}

export async function loadArtifacts(): Promise<Artifacts> {
  try {
    return await loadArtifactsFrom("control");
  } catch {
    return loadArtifactsFrom("static");
  }
}

async function loadArtifactsFrom(source: "control" | "static"): Promise<Artifacts> {
  const paths =
    source === "control"
      ? {
          catalog: "/control/catalog",
          openapi: "/control/specs/openapi",
          schedules: "/control/specs/schedules",
          docsRegistry: "/control/docs/registry",
        }
      : {
          catalog: "/.ryvus/catalog.json",
          openapi: "/.ryvus/openapi.json",
          schedules: "/.ryvus/schedules.json",
          docsRegistry: "/.ryvus/docs/registry.json",
        };

  const [catalog, openapi, schedules, docsRegistry] = await Promise.all([
    loadJson<Artifacts["catalog"]>(paths.catalog),
    loadJson<Artifacts["openapi"]>(paths.openapi),
    loadJson<Artifacts["schedules"]>(paths.schedules),
    loadJson<Artifacts["docsRegistry"]>(paths.docsRegistry),
  ]);

  validateCatalog(catalog);
  validateOpenApi(openapi);
  validateSchedules(schedules);
  validateDocsRegistry(docsRegistry);

  return { catalog, openapi, schedules, docsRegistry };
}

export async function loadDocPage(page: DocsRegistryPage): Promise<string> {
  if (!page.content_path) {
    return JSON.stringify(page, null, 2);
  }

  return loadText(page.content_path);
}
