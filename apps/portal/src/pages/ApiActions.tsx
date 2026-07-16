import { useEffect, useMemo, useState, type ReactNode } from "react";
import { defaultGatewayUrl, gatewayUrl } from "../api/runtime";
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
  responses?: Record<string, OpenApiResponse>;
  security?: Array<Record<string, unknown>>;
  "x-ryvus-authorizer"?: string;
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

type OpenApiResponse = {
  description?: string;
  content?: unknown;
};

type SchemaField = {
  name: string;
  type: string;
  required: boolean;
  description: string;
};

type HeaderRow = {
  id: number;
  name: string;
  value: string;
};

type FormRow = {
  id: number;
  name: string;
  value: string;
};

type RequestContent = {
  mediaType: string;
  schema?: unknown;
};

type SecurityScheme = {
  type?: string;
  scheme?: string;
  in?: string;
  name?: string;
};

type AuthControl = {
  id: string;
  label: string;
  location: "header" | "query" | "cookie";
  name: string;
  required: boolean;
  placeholder: string;
};

const HTTP_METHODS = ["get", "post", "put", "patch", "delete", "head", "options"];

export function ApiActions({ artifacts }: { artifacts: Artifacts }) {
  const operations = useMemo(() => openApiOperations(artifacts.openapi), [artifacts.openapi]);
  const routeGroups = useMemo(() => groupRoutes(operations), [operations]);
  const [selectedKey, setSelectedKey] = useState(operations[0]?.key ?? "");
  const selected = operations.find((operation) => operation.key === selectedKey) ?? operations[0];
  const selectedAction = selected && artifacts.catalog.actions.find((action) => {
    const api = (action.kind as { Api?: { method?: string; path?: string } }).Api;
    return api?.method?.toLowerCase() === selected.method && api.path === selected.path;
  });

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
    <Page
      eyebrow="OpenAPI"
      title="Gateway"
      actions={
        <div className="flex gap-2"><a
          className="inline-flex min-h-9 items-center justify-center rounded-md border border-white/10 bg-white/[0.04] px-3 font-mono text-xs font-bold text-slate-300 transition hover:bg-white/[0.08] hover:text-white"
          href={gatewayUrl("/openapi.json")}
          target="_blank"
          rel="noreferrer"
        >
          openapi.json
        </a>{selectedAction && <a className="inline-flex min-h-9 items-center rounded-md border border-white/10 px-3 font-mono text-xs font-bold text-slate-300 hover:text-white" href={`#execution-preview?action_id=${encodeURIComponent(selectedAction.name ?? selectedAction.entrypoint)}&action_revision=${encodeURIComponent(actionRevision(selectedAction))}`}>History</a>}</div>
      }
    >
      <div className="grid min-w-0 gap-4 xl:grid-cols-[340px_minmax(0,1fr)]">
        <Panel className="min-w-0 self-start" aria-label="Gateway routes">
          <div className="flex items-center justify-between border-b border-white/10 px-4 py-3">
            <h2 className="font-mono text-xs font-bold uppercase text-slate-400">Routes</h2>
            <Badge tone="slate">{operations.length}</Badge>
          </div>
          <div className="grid gap-3 p-3">
            {routeGroups.map((group) => (
              <div key={group.segment} className="grid gap-1.5">
                <div className="flex items-center gap-2 px-1 font-mono text-[11px] font-bold uppercase text-slate-600">
                  <span className="h-px flex-1 bg-white/10" />
                  <code>/{group.segment}</code>
                </div>
                <div className="grid gap-1 border-l border-white/10 pl-2">
                  {group.operations.map((operation) => (
                    <button
                      key={operation.key}
                      type="button"
                      className={cn(
                        "grid w-full grid-cols-[64px_minmax(0,1fr)] gap-3 rounded-md border border-transparent px-2.5 py-2 text-left transition hover:border-white/10 hover:bg-white/[0.04]",
                        operation.key === selected.key && "border-white/10 bg-[#17181c] shadow-[inset_2px_0_0_#6f3dff]",
                      )}
                      onClick={() => setSelectedKey(operation.key)}
                    >
                      <MethodBadge method={operation.method} />
                      <span className="min-w-0">
                        <strong className="block truncate text-sm font-semibold text-slate-100">
                          {operation.operation.summary || operation.operation.operationId || operation.path}
                        </strong>
                        <code className="block truncate text-xs text-slate-500">{routeTail(operation.path)}</code>
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </Panel>
        <TryIt operation={selected} openapi={artifacts.openapi} />
      </div>
    </Page>
  );
}

function actionRevision(action: Artifacts["catalog"]["actions"][number]) {
  const bytes = new TextEncoder().encode(JSON.stringify(action));
  let hash = 0xcbf29ce484222325n;
  for (const byte of bytes) hash = ((hash ^ BigInt(byte)) * 0x100000001b3n) & 0xffffffffffffffffn;
  return `action-definition-v1:${hash.toString(16).padStart(16, "0")}`;
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

function TryIt({ operation, openapi }: { operation: Operation; openapi: Artifacts["openapi"] }) {
  const baseUrl = defaultGatewayUrl();
  const [pathValues, setPathValues] = useState<Record<string, string>>({});
  const [queryValues, setQueryValues] = useState<Record<string, string>>({});
  const [authValues, setAuthValues] = useState<Record<string, string>>({});
  const [headers, setHeaders] = useState<HeaderRow[]>([{ id: 1, name: "", value: "" }]);
  const [body, setBody] = useState("");
  const [formRows, setFormRows] = useState<FormRow[]>([{ id: 1, name: "", value: "" }]);
  const [response, setResponse] = useState<TryResponse | null>(null);
  const [error, setError] = useState("");
  const [isRunning, setIsRunning] = useState(false);

  const pathParams = useMemo(() => pathParameters(operation), [operation]);
  const queryParams = useMemo(() => queryParameters(operation), [operation]);
  const authControls = useMemo(() => authorizerControls(operation, openapi), [operation, openapi]);
  const parameterControls = useMemo(() => authorizerParameterControls(operation), [operation]);
  const specPathParams = useMemo(() => pathParameterSpecs(operation), [operation]);
  const allParameters = useMemo(
    () => [
      ...pathParams.map((name) =>
        specPathParams.find((param) => param.name === name) ?? { name, in: "path", required: true, schema: { type: "string" } },
      ),
      ...(operation.operation.parameters ?? []).filter((param) => param.in !== "path"),
    ],
    [operation.operation.parameters, pathParams, specPathParams],
  );
  const requestContentOptions = requestContents(operation);
  const requestContent = requestContentOptions[0];
  const responseContentTypes = responseMediaTypes(operation);
  const requestSchema = requestContent?.schema;
  const effectiveQueryValues = useMemo(
    () => ({ ...queryValues, ...authQueryValues(authControls, authValues) }),
    [queryValues, authControls, authValues],
  );
  const requestUrl = buildUrl(baseUrl, operation.path, pathValues, effectiveQueryValues);
  const encodedBody = encodeRequestBody(requestContent?.mediaType, body, formRows);
  const effectiveHeaderValues = useMemo(
    () => ({ ...authHeaderValues(authControls, authValues), ...authHeaderValues(parameterControls, authValues) }),
    [authControls, parameterControls, authValues],
  );
  const curl = buildCurl(operation.method, requestUrl, requestContent?.mediaType, encodedBody ?? "", [
    ...Object.entries(effectiveHeaderValues),
    ...headers
      .filter((header) => header.name.trim())
      .map((header) => [header.name.trim(), header.value] as [string, string]),
  ]);
  const requestValidationError = requestValidationMessage(
    pathParams,
    pathValues,
    queryParams,
    queryValues,
    requestContent,
    body,
    formRows,
    [...authControls, ...parameterControls],
    authValues,
  );
  const bodyInvalid = Boolean(requestContent && isMissingRequestBody(requestContent, body, formRows));

  useEffect(() => {
    setPathValues(Object.fromEntries(pathParameters(operation).map((name) => [name, ""])));
    setQueryValues(Object.fromEntries(queryParameters(operation).map((param) => [param.name, ""])));
    setAuthValues(Object.fromEntries([...authorizerControls(operation, openapi), ...authorizerParameterControls(operation)].map((control) => [control.id, ""])));
    setHeaders([{ id: 1, name: "", value: "" }]);
    setBody(sampleBodyText(operation));
    setFormRows(sampleFormRows(operation));
    setResponse(null);
    setError("");
  }, [operation, openapi]);

  async function sendRequest() {
    if (requestValidationError) {
      setError(requestValidationError);
      return;
    }

    setIsRunning(true);
    setError("");
    setResponse(null);

    const startedAt = performance.now();
    try {
      const requestHeaders = new Headers();
      for (const [name, value] of Object.entries(effectiveHeaderValues)) {
        if (value.trim()) {
          requestHeaders.set(name, value);
        }
      }
      const cookie = authCookieValue([...authControls, ...parameterControls], authValues);
      if (cookie) {
        requestHeaders.set("cookie", cookie);
      }
      for (const header of headers) {
        if (header.name.trim()) {
          requestHeaders.set(header.name.trim(), header.value);
        }
      }

      if (requestContent && encodedBody !== undefined) {
        requestHeaders.set("content-type", requestContent.mediaType);
      }

      let requestBody = encodedBody;
      if (requestContent?.mediaType === "application/json" && requestBody?.trim()) {
        JSON.parse(body);
      }

      const result = await fetch(requestUrl, {
        method: operation.method,
        headers: requestHeaders,
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
    <div className="grid min-w-0 gap-4">
      <Panel className="grid gap-0">
        <div className="grid gap-3 border-b border-white/10 px-5 py-4">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div className="flex min-w-0 items-start gap-3">
              <MethodBadge method={operation.method} />
              <div className="min-w-0">
                <h2 className="truncate text-lg font-semibold text-white">
                  {operation.operation.summary || operation.operation.operationId || operation.path}
                </h2>
                <code className="block truncate text-xs text-slate-500">{operation.path}</code>
              </div>
            </div>
            <div className="grid justify-items-start gap-2 sm:justify-items-end">
              <div className="grid justify-items-start gap-1 sm:justify-items-end">
                <ContentTypeBadges label="Consumes" values={requestContentOptions.map((content) => content.mediaType)} />
                <ContentTypeBadges label="Produces" values={responseContentTypes} />
              </div>
              <Button type="button" onClick={sendRequest} disabled={isRunning || Boolean(requestValidationError)}>
                {isRunning ? "Sending..." : "Run endpoint"}
              </Button>
            </div>
          </div>
        </div>

        <div className="grid gap-4 p-5">
          <ContractSection title="Try it">
            <div className="grid gap-4 xl:grid-cols-[minmax(0,0.9fr)_minmax(320px,0.7fr)]">
              <div className="grid gap-4">
                <div className="grid gap-4">
                  {(authControls.length > 0 || parameterControls.length > 0) && (
                    <AuthorizationEditor
                      authorizer={operation.operation["x-ryvus-authorizer"]}
                      securityControls={authControls}
                      parameterControls={parameterControls}
                      values={authValues}
                      onChange={(id, value) =>
                        setAuthValues((current) => ({ ...current, [id]: value }))
                      }
                    />
                  )}

                  {pathParams.length > 0 && (
                    <div className="grid gap-3 sm:grid-cols-2">
                      {pathParams.map((name) => {
                        const invalid = isMissingRequiredValue(pathValues[name]);
                        const spec = specPathParams.find((param) => param.name === name);
                        const type = schemaType(spec?.schema ?? { type: "string" });

                        return (
                        <label className="grid gap-2" key={name}>
                          <span className={cn("flex items-center gap-2 font-mono text-[11px] font-bold uppercase", invalid ? "text-red-300" : "text-slate-500")}>
                            <span>Path: {name} *</span>
                            <ParameterType type={type} />
                          </span>
                          <input
                            aria-invalid={invalid}
                            required
                            className={cn(invalid && "!border-red-400/70 !shadow-[0_0_0_1px_rgba(248,113,113,0.32)]")}
                            placeholder={type}
                            value={pathValues[name] ?? ""}
                            onChange={(event) =>
                              setPathValues((values) => ({ ...values, [name]: event.target.value }))
                            }
                          />
                        </label>
                        );
                      })}
                    </div>
                  )}

                  {queryParams.length > 0 && (
                    <div className="grid gap-3 sm:grid-cols-2">
                      {queryParams.map((param) => {
                        const invalid = Boolean(param.required && isMissingRequiredValue(queryValues[param.name]));
                        const type = schemaType(param.schema);

                        return (
                        <label className="grid gap-2" key={param.name}>
                          <span className={cn("flex items-center gap-2 font-mono text-[11px] font-bold uppercase", invalid ? "text-red-300" : "text-slate-500")}>
                            <span>
                              Query: {param.name}
                              {param.required ? " *" : ""}
                            </span>
                            <ParameterType type={type} />
                          </span>
                          <input
                            aria-invalid={invalid}
                            required={param.required}
                            className={cn(invalid && "!border-red-400/70 !shadow-[0_0_0_1px_rgba(248,113,113,0.32)]")}
                            placeholder={type}
                            value={queryValues[param.name] ?? ""}
                            onChange={(event) =>
                              setQueryValues((values) => ({ ...values, [param.name]: event.target.value }))
                            }
                          />
                        </label>
                        );
                      })}
                    </div>
                  )}

                  {requestContent && (
                    <RequestBodyEditor
                      content={requestContent}
                      body={body}
                      onBodyChange={setBody}
                      formRows={formRows}
                      onFormRowsChange={setFormRows}
                      invalid={bodyInvalid}
                    />
                  )}

                  <details className="text-sm text-slate-300">
                    <summary className="cursor-pointer font-medium">Headers</summary>
                    <HeaderEditor headers={headers} onChange={setHeaders} />
                  </details>
                </div>

                <details className="text-sm text-slate-300">
                  <summary className="cursor-pointer font-medium">cURL</summary>
                  <CodeBlock className="mt-2">{curl}</CodeBlock>
                </details>
              </div>

              <div className="grid content-start gap-4">
                <div className="flex items-center justify-between gap-3">
                  <h4 className="font-mono text-[11px] font-bold uppercase text-slate-500">Response</h4>
                  {response ? (
                    <span className={cn("font-mono text-xs font-bold", responseStatusClass(response.status))}>
                      {response.status} {response.statusText} / {response.durationMs}ms
                    </span>
                  ) : error ? (
                    <span className="font-mono text-xs font-bold text-red-300">failed</span>
                  ) : (
                    <span className="font-mono text-xs text-slate-600">idle</span>
                  )}
                </div>
                {error ? (
                  <p className="rounded-md border border-red-400/20 bg-red-500/10 p-3 text-sm text-red-200">{error}</p>
                ) : response ? (
                  <>
                    <CodeBlock>{response.body}</CodeBlock>
                    <details className="min-w-0 text-sm text-slate-300">
                      <summary className="cursor-pointer font-medium">Headers</summary>
                      <CodeBlock className="mt-2 max-w-full">{response.headers}</CodeBlock>
                    </details>
                  </>
                ) : (
                  <div className="grid min-h-52 place-items-center rounded-md border border-dashed border-white/10 bg-[#050506] p-6 text-center">
                    <p className="max-w-xs text-sm leading-6 text-slate-500">
                      Run the endpoint to inspect status, headers, and response body.
                    </p>
                  </div>
                )}
              </div>
            </div>
          </ContractSection>

          <div className="grid gap-4">
            {operation.operation.description && (
              <p className="max-w-3xl text-sm leading-6 text-slate-400">{operation.operation.description}</p>
            )}
            <ContractSection title="Parameters">
              <ParametersTable parameters={allParameters} />
            </ContractSection>
            <ContractSection title="Request body">
              <SchemaEntity label="Request body" schema={requestSchema} />
            </ContractSection>
            <ContractSection title="Responses">
              <ResponsesList operation={operation} />
            </ContractSection>
          </div>
        </div>
      </Panel>
    </div>
  );
}

function HeaderEditor({
  headers,
  onChange,
}: {
  headers: HeaderRow[];
  onChange: (headers: HeaderRow[]) => void;
}) {
  function update(id: number, field: "name" | "value", value: string) {
    onChange(headers.map((header) => header.id === id ? { ...header, [field]: value } : header));
  }

  function add() {
    onChange([...headers, { id: Math.max(0, ...headers.map((header) => header.id)) + 1, name: "", value: "" }]);
  }

  function remove(id: number) {
    const next = headers.filter((header) => header.id !== id);
    onChange(next.length > 0 ? next : [{ id: 1, name: "", value: "" }]);
  }

  return (
    <div className="mt-2 grid gap-2">
      <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_32px] gap-2 font-mono text-[11px] font-bold uppercase text-slate-500">
        <span>Header</span>
        <span>Value</span>
        <span />
      </div>
      {headers.map((header) => (
        <div key={header.id} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_32px] gap-2">
          <input
            value={header.name}
            onChange={(event) => update(header.id, "name", event.target.value)}
            placeholder="x-customer-id"
          />
          <input
            value={header.value}
            onChange={(event) => update(header.id, "value", event.target.value)}
            placeholder="cus_123"
          />
          <button
            type="button"
            className="min-h-9 rounded-md border border-white/10 bg-white/[0.04] text-sm font-semibold text-slate-400 transition hover:bg-white/[0.08] hover:text-white"
            onClick={() => remove(header.id)}
            aria-label="Remove header"
          >
            -
          </button>
        </div>
      ))}
      <button
        type="button"
        className="justify-self-start rounded-md border border-white/10 bg-white/[0.04] px-3 py-1.5 text-sm font-semibold text-slate-300 transition hover:bg-white/[0.08] hover:text-white"
        onClick={add}
      >
        Add header
      </button>
    </div>
  );
}

function AuthorizationEditor({
  authorizer,
  securityControls,
  parameterControls,
  values,
  onChange,
}: {
  authorizer?: string;
  securityControls: AuthControl[];
  parameterControls: AuthControl[];
  values: Record<string, string>;
  onChange: (id: string, value: string) => void;
}) {
  return (
    <div className="grid gap-3 rounded-md border border-violet-400/20 bg-violet-500/[0.06] p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <h4 className="font-mono text-[11px] font-bold uppercase text-violet-200">Authorizer</h4>
          {authorizer && <code className="truncate text-xs text-violet-100/80">{authorizer}</code>}
        </div>
        <Badge tone="violet">{securityControls.length} scheme{securityControls.length === 1 ? "" : "s"}</Badge>
      </div>

      {securityControls.length > 0 && (
        <div className="grid gap-3 sm:grid-cols-2">
          {securityControls.map((control) => (
            <AuthInput
              key={control.id}
              control={control}
              value={values[control.id] ?? ""}
              onChange={onChange}
            />
          ))}
        </div>
      )}

      {parameterControls.length > 0 && (
        <div className="grid gap-3 border-t border-white/10 pt-3 sm:grid-cols-2">
          {parameterControls.map((control) => (
            <AuthInput
              key={control.id}
              control={control}
              value={values[control.id] ?? ""}
              onChange={onChange}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function AuthInput({
  control,
  value,
  onChange,
}: {
  control: AuthControl;
  value: string;
  onChange: (id: string, value: string) => void;
}) {
  const invalid = control.required && isMissingRequiredValue(value);

  return (
    <label className="grid gap-2">
      <span className={cn("flex items-center gap-2 font-mono text-[11px] font-bold uppercase", invalid ? "text-red-300" : "text-slate-500")}>
        <span>
          {control.label}
          {control.required ? " *" : ""}
        </span>
        <ParameterType type={control.location} />
      </span>
      <input
        aria-invalid={invalid}
        required={control.required}
        className={cn(invalid && "!border-red-400/70 !shadow-[0_0_0_1px_rgba(248,113,113,0.32)]")}
        placeholder={control.placeholder}
        value={value}
        onChange={(event) => onChange(control.id, event.target.value)}
      />
    </label>
  );
}

function ContentTypeBadges({ label, values }: { label: string; values: string[] }) {
  const uniqueValues = Array.from(new Set(values));
  if (uniqueValues.length === 0) {
    return null;
  }

  return (
    <div className="flex flex-wrap items-center gap-1.5 sm:justify-end">
      <span className="font-mono text-[11px] font-bold uppercase text-slate-600">{label}</span>
      {uniqueValues.map((value) => (
        <span
          key={`${label}-${value}`}
          className="rounded-md border border-white/10 bg-white/[0.04] px-2 py-0.5 font-mono text-[11px] font-semibold text-slate-400"
        >
          {value}
        </span>
      ))}
    </div>
  );
}

function ParameterType({ type }: { type: string }) {
  return (
    <span className="rounded border border-white/10 bg-white/[0.04] px-1.5 py-0.5 text-[10px] text-slate-500">
      {type}
    </span>
  );
}

function RequestBodyEditor({
  content,
  body,
  onBodyChange,
  formRows,
  onFormRowsChange,
  invalid,
}: {
  content: RequestContent;
  body: string;
  onBodyChange: (value: string) => void;
  formRows: FormRow[];
  onFormRowsChange: (rows: FormRow[]) => void;
  invalid: boolean;
}) {
  if (content.mediaType === "application/x-www-form-urlencoded") {
    return <FormEditor rows={formRows} onChange={onFormRowsChange} invalid={invalid} />;
  }

  return (
    <label className="grid gap-2">
      <span className={cn("font-mono text-[11px] font-bold uppercase", invalid ? "text-red-300" : "text-slate-500")}>
        {content.mediaType === "text/plain" ? "Text body" : "JSON body"} *
      </span>
      <textarea
        aria-invalid={invalid}
        required
        className={cn(invalid && "!border-red-400/70 !shadow-[0_0_0_1px_rgba(248,113,113,0.32)]")}
        value={body}
        onChange={(event) => onBodyChange(event.target.value)}
        rows={8}
      />
    </label>
  );
}

function FormEditor({
  rows,
  onChange,
  invalid,
}: {
  rows: FormRow[];
  onChange: (rows: FormRow[]) => void;
  invalid: boolean;
}) {
  function update(id: number, field: "name" | "value", value: string) {
    onChange(rows.map((row) => row.id === id ? { ...row, [field]: value } : row));
  }

  function add() {
    onChange([...rows, { id: Math.max(0, ...rows.map((row) => row.id)) + 1, name: "", value: "" }]);
  }

  function remove(id: number) {
    const next = rows.filter((row) => row.id !== id);
    onChange(next.length > 0 ? next : [{ id: 1, name: "", value: "" }]);
  }

  return (
    <div className="grid gap-2">
      <div className={cn("grid grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_32px] gap-2 font-mono text-[11px] font-bold uppercase", invalid ? "text-red-300" : "text-slate-500")}>
        <span>Field</span>
        <span>Value *</span>
        <span />
      </div>
      {rows.map((row) => (
        <div key={row.id} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_32px] gap-2">
          <input
            aria-invalid={invalid}
            className={cn(invalid && "!border-red-400/70 !shadow-[0_0_0_1px_rgba(248,113,113,0.32)]")}
            value={row.name}
            onChange={(event) => update(row.id, "name", event.target.value)}
            placeholder="name"
          />
          <input
            aria-invalid={invalid}
            required
            className={cn(invalid && "!border-red-400/70 !shadow-[0_0_0_1px_rgba(248,113,113,0.32)]")}
            value={row.value}
            onChange={(event) => update(row.id, "value", event.target.value)}
            placeholder="value"
          />
          <button
            type="button"
            className="min-h-9 rounded-md border border-white/10 bg-white/[0.04] text-sm font-semibold text-slate-400 transition hover:bg-white/[0.08] hover:text-white"
            onClick={() => remove(row.id)}
            aria-label="Remove field"
          >
            -
          </button>
        </div>
      ))}
      <button
        type="button"
        className="justify-self-start rounded-md border border-white/10 bg-white/[0.04] px-3 py-1.5 text-sm font-semibold text-slate-300 transition hover:bg-white/[0.08] hover:text-white"
        onClick={add}
      >
        Add field
      </button>
    </div>
  );
}

function ContractSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="rounded-md border border-white/10 bg-[#0b0c0e]">
      <div className="border-b border-white/10 px-4 py-3">
        <h3 className="font-mono text-[11px] font-bold uppercase text-slate-500">{title}</h3>
      </div>
      <div className="grid gap-3 p-4">{children}</div>
    </div>
  );
}

function ParametersTable({ parameters }: { parameters: OpenApiParameter[] }) {
  if (parameters.length === 0) {
    return <p className="text-sm text-slate-500">No parameters</p>;
  }

  return (
    <div className="overflow-hidden rounded-md border border-white/10">
      <div className="grid grid-cols-[minmax(0,1fr)_72px_78px_100px] gap-3 border-b border-white/10 bg-[#050506] px-3 py-2 font-mono text-[11px] font-bold uppercase text-slate-600">
        <span>Name</span>
        <span>In</span>
        <span>Required</span>
        <span>Type</span>
      </div>
      {parameters.map((parameter) => (
        <div key={`${parameter.in}-${parameter.name}`} className="grid grid-cols-[minmax(0,1fr)_72px_78px_100px] gap-3 border-b border-white/10 px-3 py-2 text-xs last:border-b-0">
          <code className="truncate text-slate-200">{parameter.name}</code>
          <span className="text-slate-400">{parameter.in ?? "-"}</span>
          <span className={parameter.required ? "text-amber-200" : "text-slate-500"}>
            {parameter.required ? "yes" : "no"}
          </span>
          <span className="truncate text-slate-400">{schemaType(parameter.schema)}</span>
        </div>
      ))}
    </div>
  );
}

function SchemaEntity({ label, schema }: { label: string; schema?: unknown }) {
  if (!schema) {
    return <p className="text-sm text-slate-500">No {label.toLowerCase()}</p>;
  }

  const fields = schemaFields(schema);

  return (
    <div className="grid gap-3">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h4 className="text-sm font-semibold text-white">{schemaTitle(schema, label)}</h4>
          <p className="mt-1 font-mono text-[11px] uppercase text-slate-600">{schemaType(schema)}</p>
        </div>
        <Badge tone="slate">{fields.length} field{fields.length === 1 ? "" : "s"}</Badge>
      </div>
      <div className="overflow-hidden rounded-md border border-white/10">
        {fields.map((field) => (
          <div key={field.name} className="grid gap-1 border-b border-white/10 px-3 py-2 text-xs last:border-b-0 sm:grid-cols-[minmax(0,1fr)_120px_72px] sm:gap-3">
            <code className="truncate text-slate-200">{field.name}</code>
            <span className="truncate text-slate-400">{field.type}</span>
            <span className={field.required ? "text-amber-200" : "text-slate-500"}>
              {field.required ? "required" : "optional"}
            </span>
            {field.description && <p className="text-slate-500 sm:col-span-3">{field.description}</p>}
          </div>
        ))}
      </div>
      <details className="text-sm text-slate-300">
        <summary className="cursor-pointer font-medium">Example JSON</summary>
        <CodeBlock className="mt-2">{JSON.stringify(sampleFromSchema(schema), null, 2)}</CodeBlock>
      </details>
    </div>
  );
}

function ResponsesList({ operation }: { operation: Operation }) {
  const responses = responseEntries(operation);
  if (responses.length === 0) {
    return <p className="text-sm text-slate-500">No responses</p>;
  }

  return (
    <div className="grid gap-3">
      {responses.map((response) => {
        const isSuccess = response.status.startsWith("2");
        const content = (
          <>
            {response.description && <p className="mt-2 text-sm text-slate-400">{response.description}</p>}
            {response.schema !== undefined && (
              <div className="mt-3 border-t border-white/10 pt-3">
                <SchemaEntity label={`Response ${response.status}`} schema={response.schema} />
              </div>
            )}
          </>
        );

        return (
          <details key={response.status} className="rounded-md border border-white/10 bg-[#050506] p-3" open={isSuccess}>
            <summary className="flex cursor-pointer items-center gap-2">
              <Badge tone={responseTone(response.status)}>{response.status}</Badge>
              <span className="text-sm text-slate-400">{response.description || "Response"}</span>
            </summary>
            <div className="pt-3">
              {content}
            </div>
          </details>
        );
      })}
    </div>
  );
}

function responseTone(status: string): "green" | "red" | "slate" {
  return status.startsWith("2") ? "green" : status.startsWith("4") || status.startsWith("5") ? "red" : "slate";
}

function responseStatusClass(status: number) {
  if (status >= 200 && status < 300) {
    return "text-emerald-300";
  }
  if (status >= 300 && status < 400) {
    return "text-amber-200";
  }
  if (status >= 400) {
    return "text-red-300";
  }
  return "text-slate-400";
}

function MethodBadge({ method }: { method: string }) {
  const tone =
    method === "DELETE"
      ? "red"
      : method === "GET"
        ? "green"
      : method === "POST"
        ? "violet"
        : method === "PUT" || method === "PATCH"
          ? "amber"
          : "slate";
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

function pathParameterSpecs(operation: Operation): OpenApiParameter[] {
  return (operation.operation.parameters ?? []).filter((param) => param.in === "path");
}

function queryParameters(operation: Operation): OpenApiParameter[] {
  return (operation.operation.parameters ?? []).filter((param) => param.in === "query");
}

function authorizerParameterControls(operation: Operation): AuthControl[] {
  return (operation.operation.parameters ?? [])
    .filter((param) => param.in === "header" || param.in === "cookie")
    .map((param) => ({
      id: `parameter:${param.in}:${param.name}`,
      label: `${param.in === "header" ? "Header" : "Cookie"}: ${param.name}`,
      location: param.in as "header" | "cookie",
      name: param.name,
      required: Boolean(param.required),
      placeholder: schemaType(param.schema),
    }));
}

function authorizerControls(operation: Operation, openapi: Artifacts["openapi"]): AuthControl[] {
  const schemes = securitySchemes(openapi);
  const names = (operation.operation.security ?? [])
    .flatMap((requirement) => Object.keys(requirement));

  return names.flatMap((name) => {
    const scheme = schemes[name];
    if (!scheme) {
      return [];
    }

    if (scheme.type === "http" && scheme.scheme?.toLowerCase() === "bearer") {
      return [{
        id: `security:${name}`,
        label: "Authorization",
        location: "header" as const,
        name: "Authorization",
        required: true,
        placeholder: "Bearer dev",
      }];
    }

    if (scheme.type === "apiKey" && isAuthLocation(scheme.in) && scheme.name) {
      return [{
        id: `security:${name}`,
        label: `${scheme.in === "header" ? "Header" : scheme.in === "query" ? "Query" : "Cookie"}: ${scheme.name}`,
        location: scheme.in,
        name: scheme.name,
        required: true,
        placeholder: "value",
      }];
    }

    return [];
  });
}

function securitySchemes(openapi: Artifacts["openapi"]): Record<string, SecurityScheme> {
  const components = asRecord(openapi.components);
  const schemes = asRecord(components.securitySchemes);

  return Object.fromEntries(
    Object.entries(schemes)
      .filter((entry): entry is [string, Record<string, unknown>] => isRecord(entry[1]))
      .map(([name, value]) => [
        name,
        {
          type: typeof value.type === "string" ? value.type : undefined,
          scheme: typeof value.scheme === "string" ? value.scheme : undefined,
          in: typeof value.in === "string" ? value.in : undefined,
          name: typeof value.name === "string" ? value.name : undefined,
        },
      ]),
  );
}

function isAuthLocation(value: string | undefined): value is "header" | "query" | "cookie" {
  return value === "header" || value === "query" || value === "cookie";
}

function requestJsonSchema(operation: Operation): unknown | undefined {
  return requestContents(operation)[0]?.schema;
}

function requestContents(operation: Operation): RequestContent[] {
  const content = asRecord(asRecord(operation.operation.requestBody).content);
  return Object.entries(content).map(([mediaType, value]) => ({
    mediaType,
    schema: asRecord(value).schema,
  }));
}

function responseEntries(operation: Operation): Array<{ status: string; description: string; schema?: unknown }> {
  return Object.entries(operation.operation.responses ?? {})
    .sort(([left], [right]) => left.localeCompare(right, undefined, { numeric: true }))
    .map(([status, response]) => ({
      status,
      description: response.description ?? "",
      schema: findJsonSchema(response),
    }));
}

function responseMediaTypes(operation: Operation): string[] {
  return Array.from(new Set(
    Object.values(operation.operation.responses ?? {})
      .flatMap((response) => Object.keys(asRecord(response.content))),
  ));
}

function findJsonSchema(value: unknown): unknown | undefined {
  const content = asRecord(asRecord(value).content);
  const jsonContent = asRecord(content["application/json"]);
  return jsonContent.schema;
}

function schemaTitle(schema: unknown, fallback: string): string {
  const object = asRecord(schema);
  if (typeof object.title === "string" && object.title.trim()) {
    return object.title;
  }
  if (typeof object.$ref === "string") {
    return object.$ref.split("/").filter(Boolean).at(-1) ?? fallback;
  }
  return fallback;
}

function schemaType(schema: unknown): string {
  const object = asRecord(schema);
  if (typeof object.$ref === "string") {
    return object.$ref.split("/").filter(Boolean).at(-1) ?? "ref";
  }
  if (object.type === "array") {
    return `array<${schemaType(object.items)}>`;
  }
  if (typeof object.type === "string") {
    return object.type;
  }
  if (object.properties) {
    return "object";
  }
  return "unknown";
}

function schemaFields(schema: unknown): SchemaField[] {
  const object = asRecord(schema);
  const item = object.type === "array" ? asRecord(object.items) : object;
  const properties = asRecord(item.properties);
  const required = new Set(
    Array.isArray(item.required) ? item.required.filter((name): name is string => typeof name === "string") : [],
  );

  if (Object.keys(properties).length === 0) {
    return [{
      name: schemaTitle(schema, "value"),
      type: schemaType(schema),
      required: false,
      description: typeof item.description === "string" ? item.description : "",
    }];
  }

  return Object.entries(properties).map(([name, child]) => {
    const childObject = asRecord(child);
    return {
      name,
      type: schemaType(child),
      required: required.has(name),
      description: typeof childObject.description === "string" ? childObject.description : "",
    };
  });
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

function requestValidationMessage(
  pathParams: string[],
  pathValues: Record<string, string>,
  queryParams: OpenApiParameter[],
  queryValues: Record<string, string>,
  requestContent: RequestContent | undefined,
  body: string,
  formRows: FormRow[],
  authControls: AuthControl[],
  authValues: Record<string, string>,
) {
  for (const name of pathParams) {
    if (!pathValues[name]?.trim()) {
      return `Path parameter ${name} is required.`;
    }
  }

  for (const param of queryParams) {
    if (param.required && !queryValues[param.name]?.trim()) {
      return `Query parameter ${param.name} is required.`;
    }
  }

  if (requestContent && isMissingRequestBody(requestContent, body, formRows)) {
    return "Request body is required.";
  }

  for (const control of authControls) {
    if (control.required && !authValues[control.id]?.trim()) {
      return `${control.label} is required.`;
    }
  }

  return "";
}

function isMissingRequiredValue(value: string | undefined) {
  return !value?.trim();
}

function isMissingRequestBody(content: RequestContent, body: string, formRows: FormRow[]) {
  if (content.mediaType === "application/x-www-form-urlencoded") {
    return !formRows.some((row) => row.name.trim() && row.value.trim());
  }

  return !body.trim();
}

function buildCurl(
  method: string,
  url: string,
  mediaType: string | undefined,
  body: string,
  headers: Array<[string, string]>,
) {
  const lines = [`curl -X ${method} ${JSON.stringify(url)}`];
  for (const [name, value] of headers) {
    if (value.trim()) {
      lines.push(`  -H ${JSON.stringify(`${name}: ${value}`)}`);
    }
  }
  if (body.trim()) {
    lines.push(`  -H "content-type: ${mediaType ?? "application/json"}"`);
    lines.push(`  --data ${JSON.stringify(body)}`);
  }
  return lines.join(" \\\n");
}

function authHeaderValues(controls: AuthControl[], values: Record<string, string>): Record<string, string> {
  return Object.fromEntries(
    controls
      .filter((control) => control.location === "header")
      .map((control) => [control.name, values[control.id] ?? ""])
      .filter(([, value]) => value.trim()),
  );
}

function authQueryValues(controls: AuthControl[], values: Record<string, string>): Record<string, string> {
  return Object.fromEntries(
    controls
      .filter((control) => control.location === "query")
      .map((control) => [control.name, values[control.id] ?? ""])
      .filter(([, value]) => value.trim()),
  );
}

function authCookieValue(controls: AuthControl[], values: Record<string, string>): string {
  return controls
    .filter((control) => control.location === "cookie")
    .map((control) => [control.name, values[control.id] ?? ""] as const)
    .filter(([, value]) => value.trim())
    .map(([name, value]) => `${name}=${value}`)
    .join("; ");
}

function allowsRequestBody(method: string) {
  return ["POST", "PUT", "PATCH"].includes(method);
}

function sampleBodyText(operation: Operation) {
  const content = requestContents(operation)[0];
  if (!content) {
    return "";
  }
  if (content.mediaType === "text/plain") {
    return "hello from ryvus";
  }
  if (content.mediaType === "application/json") {
    return JSON.stringify(content.schema ? sampleFromSchema(content.schema) : {}, null, 2);
  }
  return "";
}

function sampleFormRows(operation: Operation): FormRow[] {
  const content = requestContents(operation)[0];
  if (content?.mediaType !== "application/x-www-form-urlencoded") {
    return [{ id: 1, name: "", value: "" }];
  }

  const fields = schemaFields(content.schema);
  if (fields.length === 0) {
    return [{ id: 1, name: "", value: "" }];
  }

  return fields.map((field, index) => ({
    id: index + 1,
    name: field.name,
    value: "",
  }));
}

function encodeRequestBody(mediaType: string | undefined, body: string, formRows: FormRow[]): string | undefined {
  if (mediaType === undefined) {
    return undefined;
  }
  if (mediaType === "application/x-www-form-urlencoded") {
    const params = new URLSearchParams();
    for (const row of formRows) {
      if (row.name.trim()) {
        params.set(row.name.trim(), row.value);
      }
    }
    return params.toString();
  }
  return body;
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
