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
    key: string;
    expression: string;
  };
};

export type ActionDefinition = {
  runtime: RuntimeKind;
  kind: ApiActionKind | ScheduleActionKind | Record<string, unknown>;
  source: string;
  entrypoint: string;
  name?: string;
  policy?: unknown;
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

export type FlowBranch = {
  when: string;
  next: string;
};

export type FlowStep = {
  key: string;
  action: string;
  params?: unknown;
  config?: unknown;
  next?: string;
  next_when?: FlowBranch[];
  otherwise?: string;
  on_error?: string;
};

export type FlowDefinition = {
  key: string;
  description?: string;
  version?: string;
  steps: FlowStep[];
};

export type FlowsFile = {
  flows: FlowDefinition[];
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
    components?: Record<string, unknown>;
    tags?: Array<{ name: string }>;
  };
  schedules: SchedulesFile;
  flows: FlowsFile;
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
