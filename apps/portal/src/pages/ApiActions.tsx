import { useEffect, useMemo, useState } from "react";
import { defaultGatewayUrl } from "../api/runtime";
import type { Artifacts } from "../artifacts/types";
import { Badge, Button, CodeBlock, EmptyState, Page, Panel, cn } from "../components/ui";

type OpenApiParameter = {
  name: string;
  in?: string;
  required?: boolean;
  schema?: unknown;
};

type OpenApiOperation = {
  operationId?: string;
  summary?: string;
  description?: string;
  tags?: string[];
  parameters?: OpenApiParameter[];
  requestBody?: unknown;
};

type Operation = {
  key: string;
  method: string;
  path: string;
  operation: OpenApiOperation;
};

type TryResponse = {
  status: number;
  statusText: string;
  durationMs: number;
  headers: string;
  body: string;
};

const HTTP_METHODS = ["get", "post", "put", "patch", "delete", "head", "options"];

export function ApiActions({ artifacts }: { artifacts: Artifacts }) {
  const operations = useMemo(() => openApiOperations(artifacts.openapi), [artifacts.openapi]);
  const routeGroups = useMemo(() => groupRoutes(operations), [operations]);
  const [selectedKey, setSelectedKey] = useState(operations[0]?.key ?? "");
  const selected = operations.find((operation) => operation.key === selectedKey) ?? operations[0];

  useEffect(() => {
    if (!selectedKey && operations[0]) {
      setSelectedKey(operations[0].key);
    }
  }, [operations, selectedKey]);

  if (!selected) {
    return (
      <Page eyebrow="OpenAPI" title="Gateway">
        <EmptyState
          title="No gateway routes"
          message="No HTTP gateway routes were found in this artifact snapshot."
        />
      </Page>
    );
  }

  return (
    <Page eyebrow="OpenAPI" title="Gateway">
      <div className="grid gap-4 xl:grid-cols-[360px_minmax(0,1fr)]">
        <Panel className="self-start p-4" aria-label="Gateway routes">
          <div className="mb-4 flex items-center justify-between">
            <h2 className="text-sm font-semibold text-white">Routes</h2>
            <Badge tone="slate">{operations.length}</Badge>
          </div>
          <div className="grid gap-3">
            {routeGroups.map((group) => (
              <div key={group.segment} className="grid gap-1.5">
                <div className="flex items-center gap-2 px-2 text-xs font-semibold text-slate-500">
                  <span className="h-px flex-1 bg-white/10" />
                  <code>/{group.segment}</code>
                </div>
                <div className="grid gap-1 border-l border-white/10 pl-3">
                  {group.operations.map((operation) => (
                    <button
                      key={operation.key}
                      type="button"
                      className={cn(
                        "grid w-full grid-cols-[58px_minmax(0,1fr)] gap-3 rounded-lg border border-transparent p-2.5 text-left transition hover:border-blue-400/20 hover:bg-white/[0.04]",
                        operation.key === selected.key && "border-blue-400/25 bg-blue-500/10",
                      )}
                      onClick={() => setSelectedKey(operation.key)}
                    >
                      <MethodBadge method={operation.method} />
                      <span className="min-w-0">
                        <strong className="block truncate text-sm font-semibold text-slate-100">
                          {operation.operation.summary || operation.operation.operationId || operation.path}
                        </strong>
                        <code className="block truncate text-xs text-slate-400">{routeTail(operation.path)}</code>
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </Panel>
        <TryIt operation={selected} />
      </div>
    </Page>
  );
}

function groupRoutes(operations: Operation[]) {
  const groups = new Map<string, Operation[]>();
  for (const operation of operations) {
    const segment = operation.path.split("/").filter(Boolean)[0] ?? "root";
    groups.set(segment, [...(groups.get(segment) ?? []), operation]);
  }

  return Array.from(groups, ([segment, groupOperations]) => ({
    segment,
    operations: groupOperations.sort((left, right) => left.path.localeCompare(right.path) || left.method.localeCompare(right.method)),
  })).sort((left, right) => left.segment.localeCompare(right.segment));
}

function routeTail(path: string) {
  const segments = path.split("/").filter(Boolean);
  return segments.length > 1 ? `/${segments.slice(1).join("/")}` : "/";
}

function TryIt({ operation }: { operation: Operation }) {
  const [baseUrl, setBaseUrl] = useState(defaultGatewayUrl());
  const [pathValues, setPathValues] = useState<Record<string, string>>({});
  const [queryValues, setQueryValues] = useState<Record<string, string>>({});
  const [body, setBody] = useState("");
  const [response, setResponse] = useState<TryResponse | null>(null);
  const [error, setError] = useState("");
  const [isRunning, setIsRunning] = useState(false);

  const pathParams = useMemo(() => pathParameters(operation), [operation]);
  const queryParams = useMemo(() => queryParameters(operation), [operation]);
  const allowsBody = ["POST", "PUT", "PATCH"].includes(operation.method);
  const requestUrl = buildUrl(baseUrl, operation.path, pathValues, queryValues);
  const curl = buildCurl(operation.method, requestUrl, allowsBody ? body : "");

  useEffect(() => {
    setPathValues(Object.fromEntries(pathParameters(operation).map((name) => [name, ""])));
    setQueryValues(Object.fromEntries(queryParameters(operation).map((param) => [param.name, ""])));
    setBody(allowsRequestBody(operation.method) ? JSON.stringify(sampleBody(operation), null, 2) : "");
    setResponse(null);
    setError("");
  }, [operation]);

  async function sendRequest() {
    setIsRunning(true);
    setError("");
    setResponse(null);

    const startedAt = performance.now();
    try {
      const headers = new Headers();
      let requestBody: string | undefined;
      if (allowsBody && body.trim()) {
        JSON.parse(body);
        headers.set("content-type", "application/json");
        requestBody = body;
      }

      const result = await fetch(requestUrl, {
        method: operation.method,
        headers,
        body: requestBody,
      });
      const text = await result.text();
      setResponse({
        status: result.status,
        statusText: result.statusText,
        durationMs: Math.round(performance.now() - startedAt),
        headers: formatHeaders(result.headers),
        body: prettyBody(text),
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : "request failed");
    } finally {
      setIsRunning(false);
    }
  }

  return (
    <Panel className="grid gap-4 p-5">
      <div className="flex items-start gap-3">
        <MethodBadge method={operation.method} />
        <div>
          <h2 className="text-lg font-semibold text-white">
            {operation.operation.summary || operation.operation.operationId || operation.path}
          </h2>
          <code className="text-sm text-slate-400">{operation.path}</code>
        </div>
      </div>

      {operation.operation.description && (
        <p className="max-w-3xl text-sm leading-6 text-slate-400">{operation.operation.description}</p>
      )}

      <label className="grid gap-2">
        <span className="text-xs font-semibold text-slate-300">Base URL</span>
        <input value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} />
      </label>

      {pathParams.length > 0 && (
        <div className="grid gap-3 sm:grid-cols-2">
          {pathParams.map((name) => (
            <label className="grid gap-2" key={name}>
              <span className="text-xs font-semibold text-slate-300">Path: {name}</span>
              <input
                value={pathValues[name] ?? ""}
                onChange={(event) =>
                  setPathValues((values) => ({ ...values, [name]: event.target.value }))
                }
              />
            </label>
          ))}
        </div>
      )}

      {queryParams.length > 0 && (
        <div className="grid gap-3 sm:grid-cols-2">
          {queryParams.map((param) => (
            <label className="grid gap-2" key={param.name}>
              <span className="text-xs font-semibold text-slate-300">
                Query: {param.name}
                {param.required ? " *" : ""}
              </span>
              <input
                value={queryValues[param.name] ?? ""}
                onChange={(event) =>
                  setQueryValues((values) => ({ ...values, [param.name]: event.target.value }))
                }
              />
            </label>
          ))}
        </div>
      )}

      {allowsBody && (
        <label className="grid gap-2">
          <span className="text-xs font-semibold text-slate-300">JSON Body</span>
          <textarea value={body} onChange={(event) => setBody(event.target.value)} rows={8} />
        </label>
      )}

      <div className="grid gap-3 rounded-xl border border-white/10 bg-black/20 p-3 md:grid-cols-[minmax(0,1fr)_auto] md:items-center">
        <code className="truncate text-xs text-slate-300">{requestUrl}</code>
        <Button type="button" onClick={sendRequest} disabled={isRunning}>
          {isRunning ? "Sending..." : "Send request"}
        </Button>
      </div>

      <div className="grid gap-2">
        <div className="text-xs font-semibold uppercase text-slate-500">cURL</div>
        <CodeBlock>{curl}</CodeBlock>
      </div>

      {(error || response) && (
        <div className="grid gap-2">
          <div className="text-xs font-semibold uppercase text-slate-500">Response</div>
          {error ? (
            <p className="rounded-lg border border-red-400/20 bg-red-500/10 p-3 text-sm text-red-200">{error}</p>
          ) : response ? (
            <>
              <div className="flex gap-3 text-sm font-medium text-emerald-300">
                <span>{response.status} {response.statusText}</span>
                <span>{response.durationMs}ms</span>
              </div>
              <CodeBlock>{response.body}</CodeBlock>
              <details className="text-sm text-slate-300">
                <summary className="cursor-pointer font-medium">Headers</summary>
                <CodeBlock className="mt-2">{response.headers}</CodeBlock>
              </details>
            </>
          ) : null}
        </div>
      )}
    </Panel>
  );
}

function MethodBadge({ method }: { method: string }) {
  const tone =
    method === "DELETE"
      ? "red"
      : method === "POST"
        ? "violet"
        : method === "PUT" || method === "PATCH"
          ? "cyan"
          : "blue";
  return <Badge tone={tone}>{method}</Badge>;
}

function openApiOperations(openapi: Artifacts["openapi"]): Operation[] {
  const paths = asRecord(openapi.paths);
  return Object.entries(paths).flatMap(([path, pathItem]) => {
    const methods = asRecord(pathItem);
    return Object.entries(methods)
      .filter(([method, value]) => HTTP_METHODS.includes(method) && isRecord(value))
      .map(([method, value]) => ({
        key: `${method.toUpperCase()} ${path}`,
        method: method.toUpperCase(),
        path,
        operation: value as OpenApiOperation,
      }));
  });
}

function pathParameters(operation: Operation): string[] {
  const fromSpec = (operation.operation.parameters ?? [])
    .filter((param) => param.in === "path")
    .map((param) => param.name);
  const fromPath = Array.from(operation.path.matchAll(/\{([^}]+)\}/g)).map((match) => match[1]);
  return Array.from(new Set([...fromSpec, ...fromPath]));
}

function queryParameters(operation: Operation): OpenApiParameter[] {
  return (operation.operation.parameters ?? []).filter((param) => param.in === "query");
}

function buildUrl(
  baseUrl: string,
  path: string,
  pathValues: Record<string, string>,
  queryValues: Record<string, string>,
) {
  const resolvedPath = Object.entries(pathValues).reduce(
    (current, [name, value]) => current.replace(`{${name}}`, encodeURIComponent(value)),
    path,
  );
  const url = `${baseUrl.replace(/\/$/, "")}${resolvedPath}`;
  const query = new URLSearchParams();
  for (const [name, value] of Object.entries(queryValues)) {
    if (value.trim()) {
      query.set(name, value);
    }
  }
  const queryString = query.toString();
  return queryString ? `${url}?${queryString}` : url;
}

function buildCurl(method: string, url: string, body: string) {
  const lines = [`curl -X ${method} ${JSON.stringify(url)}`];
  if (body.trim()) {
    lines.push(`  -H "content-type: application/json"`);
    lines.push(`  --data ${JSON.stringify(body)}`);
  }
  return lines.join(" \\\n");
}

function allowsRequestBody(method: string) {
  return ["POST", "PUT", "PATCH"].includes(method);
}

function sampleBody(operation: Operation) {
  const schema = findRequestSchema(operation.operation.requestBody);
  return schema ? sampleFromSchema(schema) : {};
}

function findRequestSchema(requestBody: unknown): unknown {
  const content = asRecord(asRecord(requestBody).content);
  const jsonContent = asRecord(content["application/json"]);
  return jsonContent.schema;
}

function sampleFromSchema(schema: unknown): unknown {
  const object = asRecord(schema);
  if (object.type === "object") {
    const properties = asRecord(object.properties);
    return Object.fromEntries(
      Object.entries(properties).map(([name, child]) => [name, sampleFromSchema(child)]),
    );
  }
  if (object.type === "array") {
    return [sampleFromSchema(object.items)];
  }
  if (object.type === "integer" || object.type === "number") {
    return 0;
  }
  if (object.type === "boolean") {
    return false;
  }
  return "";
}

function formatHeaders(headers: Headers) {
  return Array.from(headers.entries())
    .map(([name, value]) => `${name}: ${value}`)
    .join("\n");
}

function prettyBody(text: string) {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

function asRecord(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : {};
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
