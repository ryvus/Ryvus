import { useEffect, useMemo, useState } from "react";
import { defaultGatewayUrl } from "../api/runtime";
import type { Artifacts } from "../artifacts/types";

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
  const [selectedKey, setSelectedKey] = useState(operations[0]?.key ?? "");
  const selected = operations.find((operation) => operation.key === selectedKey) ?? operations[0];

  useEffect(() => {
    if (!selectedKey && operations[0]) {
      setSelectedKey(operations[0].key);
    }
  }, [operations, selectedKey]);

  if (!selected) {
    return (
      <div className="page">
        <h1>API Actions</h1>
        <p>No HTTP API actions were found in this artifact snapshot.</p>
      </div>
    );
  }

  return (
    <div className="api-workspace">
      <section className="operation-browser" aria-label="API operations">
        <div className="section-heading">
          <span className="eyebrow">OpenAPI</span>
          <h1>API Actions</h1>
        </div>
        <div className="operation-list">
          {operations.map((operation) => (
            <button
              key={operation.key}
              type="button"
              className={operation.key === selected.key ? "operation-item active" : "operation-item"}
              onClick={() => setSelectedKey(operation.key)}
            >
              <span className={`method ${operation.method.toLowerCase()}`}>
                {operation.method}
              </span>
              <span>
                <strong>{operation.operation.summary || operation.operation.operationId || operation.path}</strong>
                <code>{operation.path}</code>
              </span>
            </button>
          ))}
        </div>
      </section>
      <TryIt operation={selected} />
    </div>
  );
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
    <section className="try-panel">
      <div className="operation-header">
        <span className={`method ${operation.method.toLowerCase()}`}>{operation.method}</span>
        <div>
          <h2>{operation.operation.summary || operation.operation.operationId || operation.path}</h2>
          <code>{operation.path}</code>
        </div>
      </div>

      {operation.operation.description && (
        <p className="operation-description">{operation.operation.description}</p>
      )}

      <label className="field">
        <span>Base URL</span>
        <input value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} />
      </label>

      {pathParams.length > 0 && (
        <div className="field-grid">
          {pathParams.map((name) => (
            <label className="field" key={name}>
              <span>Path: {name}</span>
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
        <div className="field-grid">
          {queryParams.map((param) => (
            <label className="field" key={param.name}>
              <span>
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
        <label className="field">
          <span>JSON Body</span>
          <textarea value={body} onChange={(event) => setBody(event.target.value)} rows={8} />
        </label>
      )}

      <div className="request-bar">
        <code>{requestUrl}</code>
        <button type="button" onClick={sendRequest} disabled={isRunning}>
          {isRunning ? "Sending..." : "Send request"}
        </button>
      </div>

      <div className="code-panel">
        <div className="panel-title">cURL</div>
        <pre>{curl}</pre>
      </div>

      {(error || response) && (
        <div className="response-panel">
          <div className="panel-title">Response</div>
          {error ? (
            <p className="error">{error}</p>
          ) : response ? (
            <>
              <div className="response-meta">
                <span>{response.status} {response.statusText}</span>
                <span>{response.durationMs}ms</span>
              </div>
              <pre>{response.body}</pre>
              <details>
                <summary>Headers</summary>
                <pre>{response.headers}</pre>
              </details>
            </>
          ) : null}
        </div>
      )}
    </section>
  );
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
