export type RuntimeKind = "Python" | "Node" | "Rust" | string;

export type ApiActionKind = {
  Api: {
    method: string;
    path: string;
    request_schema?: unknown;
    response_schema?: unknown;
    query_params?: Array<{ name: string; required: boolean; schema: unknown }>;
  };
};

export type ScheduleActionKind = {
  Schedule: {
    expression: string;
  };
};

export type ActionDefinition = {
  runtime: RuntimeKind;
  kind: ApiActionKind | ScheduleActionKind | Record<string, unknown>;
  source: string;
  entrypoint: string;
  name?: string;
};

export type Catalog = {
  actions: ActionDefinition[];
};

export type ScheduleArtifact = {
  id?: string;
  name: string;
  expression: string;
  runtime: string;
  handler: string;
  action: string;
  enabled: boolean;
};

export type SchedulesFile = {
  schedules: ScheduleArtifact[];
};

export type DocsRegistryPage = {
  id: string;
  title: string;
  path: string;
  source: string;
  content_type: "Markdown" | "OpenApiJson" | "Json" | string;
  content_path?: string;
};

export type DocsRegistry = {
  nav: Array<{ id: string; title: string; path?: string; children: unknown[] }>;
  pages: DocsRegistryPage[];
};

export type Artifacts = {
  catalog: Catalog;
  openapi: {
    openapi: string;
    info?: {
      title?: string;
      version?: string;
    };
    paths: Record<string, unknown>;
    tags?: Array<{ name: string }>;
  };
  schedules: SchedulesFile;
  docsRegistry: DocsRegistry;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function isApiAction(
  action: ActionDefinition,
): action is ActionDefinition & { kind: ApiActionKind } {
  const kind = action.kind as Record<string, unknown>;
  return (
    isRecord(kind) &&
    isRecord(kind.Api) &&
    typeof kind.Api.method === "string" &&
    typeof kind.Api.path === "string"
  );
}
