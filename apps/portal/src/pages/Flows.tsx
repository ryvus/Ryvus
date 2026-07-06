import {
  Background,
  Controls,
  Handle,
  Position,
  ReactFlow,
  type Edge,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useMemo, useState } from "react";
import type { Artifacts, FlowDefinition, FlowStep } from "../artifacts/types";
import { Badge, Button, CodeBlock, EmptyState, Page, Panel, cn } from "../components/ui";

type ExecutionStatus = "queued" | "running" | "succeeded" | "failed" | "skipped" | "cancelled";

type StepExecution = {
  key: string;
  action: string;
  status: ExecutionStatus;
  input: unknown;
  output: unknown;
  error: string | null;
  invocation_id: string | null;
  logs: FlowStepLog[];
};

type FlowStepLog = {
  level: string;
  message: string;
  fields: unknown;
};

type FlowExecution = {
  id: string;
  flow_key: string;
  status: ExecutionStatus;
  input: unknown;
  output: unknown;
  error: string | null;
  steps: StepExecution[];
};

const nodeTypes = {
  step: StepNode,
};

export function Flows({ artifacts }: { artifacts: Artifacts }) {
  const flows = artifacts.flows.flows;
  const [selectedFlowKey, setSelectedFlowKey] = useState("");
  const [executions, setExecutions] = useState<FlowExecution[]>([]);
  const [selectedExecutionId, setSelectedExecutionId] = useState("");
  const [selectedStepKey, setSelectedStepKey] = useState("");
  const selectedFlow = flows.find((flow) => flow.key === selectedFlowKey);

  return (
    <Page
      eyebrow="FlowSpec"
      title={selectedFlow ? selectedFlow.key : "Flows"}
      actions={
        selectedFlow && (
          <Button type="button" className="bg-white/10 hover:bg-white/15" onClick={() => setSelectedFlowKey("")}>
            Back to flows
          </Button>
        )
      }
    >
      {flows.length === 0 ? (
        <EmptyState title="No flows" message="No flows were found in this artifact snapshot." />
      ) : selectedFlow ? (
        <FlowDetail
          flow={selectedFlow}
          executions={executions.filter((execution) => execution.flow_key === selectedFlow.key)}
          selectedExecutionId={selectedExecutionId}
          selectedStepKey={selectedStepKey}
          onSelectExecution={(execution) => {
            setSelectedExecutionId(execution.id);
            setSelectedStepKey(execution.steps[0]?.key ?? "");
          }}
          onSelectStep={setSelectedStepKey}
          onStartExecution={async (input) => {
            const execution = await startFlowExecution(selectedFlow.key, input);
            setExecutions((current) => upsertExecution(current, execution));
            setSelectedExecutionId(execution.id);
            setSelectedStepKey(execution.steps[0]?.key ?? "");
            void pollFlowExecution(execution.id, (updated) => {
              setExecutions((current) => upsertExecution(current, updated));
              setSelectedStepKey((current) => current || (updated.steps[0]?.key ?? ""));
            }).catch((error) => {
              setExecutions((current) => upsertExecution(current, {
                ...execution,
                status: "failed",
                error: error instanceof Error ? error.message : "Flow execution failed.",
              }));
            });
            return execution;
          }}
          onCancelExecution={async (id) => {
            const execution = await cancelFlowExecution(id);
            setExecutions((current) => upsertExecution(current, execution));
            setSelectedExecutionId(execution.id);
            setSelectedStepKey((current) => current || (execution.steps[0]?.key ?? ""));
            return execution;
          }}
          onRetryStep={async (id, stepKey) => {
            const execution = await retryFlowStep(id, stepKey);
            setExecutions((current) => upsertExecution(current, execution));
            setSelectedExecutionId(execution.id);
            setSelectedStepKey(stepKey);
            void pollFlowExecution(execution.id, (updated) => {
              setExecutions((current) => upsertExecution(current, updated));
              setSelectedStepKey(stepKey);
            });
            return execution;
          }}
        />
      ) : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {flows.map((flow) => (
            <FlowListItem
              key={flow.key}
              flow={flow}
              executions={executions.filter((execution) => execution.flow_key === flow.key)}
              onOpen={() => setSelectedFlowKey(flow.key)}
            />
          ))}
        </div>
      )}
    </Page>
  );
}

function FlowListItem({
  flow,
  executions,
  onOpen,
}: {
  flow: FlowDefinition;
  executions: FlowExecution[];
  onOpen: () => void;
}) {
  const lastExecution = executions[0];

  return (
    <Panel className="grid content-between gap-5 p-4">
      <div className="grid gap-3">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <h2 className="truncate text-base font-semibold text-white">{flow.key}</h2>
            {flow.description && (
              <p className="mt-1 max-h-12 overflow-hidden text-sm leading-6 text-slate-400">{flow.description}</p>
            )}
          </div>
          {flow.version && <Badge tone="violet">v{flow.version}</Badge>}
        </div>
        <div className="grid grid-cols-3 gap-2">
          <Metric label="Steps" value={flow.steps.length.toString()} />
          <Metric label="Runs" value={executions.length.toString()} />
          <Metric label="Last" value={lastExecution?.status ?? "none"} />
        </div>
      </div>
      <Button type="button" onClick={onOpen}>Open flow</Button>
    </Panel>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg border border-white/10 bg-black/20 p-3">
      <div className="text-[11px] font-semibold uppercase text-slate-500">{label}</div>
      <div className="mt-1 truncate text-sm font-semibold text-slate-100">{value}</div>
    </div>
  );
}

function FlowDetail({
  flow,
  executions,
  selectedExecutionId,
  selectedStepKey,
  onSelectExecution,
  onSelectStep,
  onStartExecution,
  onCancelExecution,
  onRetryStep,
}: {
  flow: FlowDefinition;
  executions: FlowExecution[];
  selectedExecutionId: string;
  selectedStepKey: string;
  onSelectExecution: (execution: FlowExecution) => void;
  onSelectStep: (key: string) => void;
  onStartExecution: (input: unknown) => Promise<FlowExecution>;
  onCancelExecution: (id: string) => Promise<FlowExecution>;
  onRetryStep: (id: string, stepKey: string) => Promise<FlowExecution>;
}) {
  const [inputText, setInputText] = useState("{\n  \"input\": true\n}");
  const [inputError, setInputError] = useState("");
  const [isStarting, setIsStarting] = useState(false);
  const [isCancelling, setIsCancelling] = useState(false);
  const [isRetryingStep, setIsRetryingStep] = useState(false);
  const [isStartOpen, setIsStartOpen] = useState(false);
  const [activeView, setActiveView] = useState<"diagram" | "source">("diagram");
  const selectedExecution =
    executions.find((execution) => execution.id === selectedExecutionId) ?? executions[0];
  const selectedStep =
    [...(selectedExecution?.steps ?? [])].reverse().find((step) => step.key === selectedStepKey) ??
    selectedExecution?.steps[0];
  const graph = useMemo(
    () => flowToGraph(flow, selectedExecution, selectedStep?.key ?? selectedStepKey, onSelectStep),
    [flow, selectedExecution, selectedStep?.key, selectedStepKey, onSelectStep],
  );

  async function startExecution() {
    try {
      const input = inputText.trim() ? JSON.parse(inputText) : {};
      setInputError("");
      setIsStarting(true);
      await onStartExecution(input);
      return true;
    } catch (error) {
      setInputError(error instanceof Error ? error.message : "Flow execution failed.");
      return false;
    } finally {
      setIsStarting(false);
    }
  }

  async function cancelExecution() {
    if (!selectedExecution || !isActiveExecution(selectedExecution)) {
      return;
    }

    setIsCancelling(true);
    try {
      await onCancelExecution(selectedExecution.id);
    } finally {
      setIsCancelling(false);
    }
  }

  async function retryStep() {
    if (!selectedExecution || !selectedStep || !canRetryStep(selectedExecution, selectedStep)) {
      return;
    }

    setIsRetryingStep(true);
    try {
      await onRetryStep(selectedExecution.id, selectedStep.key);
    } finally {
      setIsRetryingStep(false);
    }
  }

  return (
    <div className="grid gap-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h2 className="text-base font-semibold text-white">{flow.key}</h2>
          {flow.description && <p className="mt-1 max-w-2xl text-sm leading-6 text-slate-400">{flow.description}</p>}
        </div>
        {flow.version && <Badge tone="violet">v{flow.version}</Badge>}
      </div>
      <div className="grid min-w-0 items-start gap-4 xl:grid-cols-[340px_minmax(0,1fr)]">
        <ExecutionList
          executions={executions}
          selectedExecutionId={selectedExecution?.id ?? ""}
          onSelectExecution={onSelectExecution}
        />
        <div className="grid min-w-0 gap-4">
          <StartExecution
            inputText={inputText}
            inputError={inputError}
            isOpen={isStartOpen}
            onOpen={() => setIsStartOpen(true)}
            onClose={() => {
              setIsStartOpen(false);
              setInputError("");
            }}
            onInputTextChange={setInputText}
            isStarting={isStarting}
            onStart={async () => {
              if (await startExecution()) {
                setIsStartOpen(false);
              }
            }}
          />
          {selectedExecution && (
            <ExecutionSummary
              execution={selectedExecution}
              isCancelling={isCancelling}
              onCancel={cancelExecution}
            />
          )}
          <div className="overflow-hidden rounded-lg border border-white/10 bg-[#111214]">
            <div className="flex items-center gap-1 border-b border-white/10 bg-[#0b0c0e] p-2">
              {(["diagram", "source"] as const).map((view) => (
                <button
                  key={view}
                  type="button"
                  onClick={() => setActiveView(view)}
                  className={cn(
                    "rounded-md px-3 py-1.5 font-mono text-xs font-bold uppercase text-slate-500 transition hover:bg-white/5 hover:text-white",
                    activeView === view && "bg-white/10 text-white",
                  )}
                >
                  {view}
                </button>
              ))}
            </div>
            {activeView === "diagram" ? (
              <div className="ryvus-flow h-[620px] bg-[#0d0e11]">
                <ReactFlow
                  nodes={graph.nodes}
                  edges={graph.edges}
                  nodeTypes={nodeTypes}
                  fitView
                  fitViewOptions={{ padding: 0.28 }}
                  minZoom={0.18}
                  nodesDraggable={false}
                  nodesConnectable={false}
                  onInit={(instance) => {
                    requestAnimationFrame(() => {
                      const viewport = instance.getViewport();
                      void instance.setViewport({ ...viewport, x: viewport.x - 90 });
                    });
                  }}
                >
                  <Background color="rgba(148, 163, 184, 0.12)" gap={24} />
                  <Controls showInteractive={false} />
                </ReactFlow>
              </div>
            ) : (
              <CodeBlock className="h-[620px] rounded-none border-0 bg-transparent">
                {JSON.stringify(flow, null, 2)}
              </CodeBlock>
            )}
          </div>
          {selectedExecution ? (
            <div className="grid min-w-0 items-start gap-4 2xl:grid-cols-[minmax(0,1fr)_minmax(460px,0.85fr)]">
              <div className="grid min-w-0 gap-4">
                <ExecutionDetail
                  execution={selectedExecution}
                  selectedStep={selectedStep}
                  onSelectStep={onSelectStep}
                  isRetryingStep={isRetryingStep}
                  onRetryStep={retryStep}
                />
              </div>
              <WatchLog step={selectedStep} />
            </div>
          ) : (
            <EmptyState
              title="No executions"
              message="Start a flow execution to inspect input, output, step state, and timeline."
            />
          )}
        </div>
      </div>
    </div>
  );
}

function StartExecution({
  inputText,
  inputError,
  isOpen,
  onOpen,
  onClose,
  onInputTextChange,
  isStarting,
  onStart,
}: {
  inputText: string;
  inputError: string;
  isOpen: boolean;
  onOpen: () => void;
  onClose: () => void;
  onInputTextChange: (value: string) => void;
  isStarting: boolean;
  onStart: () => void;
}) {
  return (
    <>
      <div className="flex items-center justify-between gap-3 rounded-lg border border-white/10 bg-[#111214] p-4">
        <div>
          <h3 className="text-sm font-semibold text-white">Start new execution</h3>
          <p className="mt-1 text-xs leading-5 text-slate-400">Open the input editor and start a run.</p>
        </div>
        <Button type="button" onClick={onOpen}>Start execution</Button>
      </div>
      {isOpen && (
        <div className="fixed inset-0 z-50 grid items-start justify-items-center overflow-auto bg-black/70 p-4 pt-8 backdrop-blur-sm sm:pt-12">
          <div className="grid w-full max-w-3xl gap-4 rounded-lg border border-white/10 bg-[#111214] p-5 shadow-[0_24px_80px_rgba(0,0,0,0.55)]">
            <div className="flex items-start justify-between gap-3">
              <div>
                <h3 className="text-base font-semibold text-white">Start execution</h3>
                <p className="mt-1 text-sm text-slate-400">Provide JSON input for this flow run.</p>
              </div>
              <Button type="button" className="bg-white/10 hover:bg-white/15" onClick={onClose}>Close</Button>
            </div>
            <textarea
              rows={16}
              value={inputText}
              onChange={(event) => onInputTextChange(event.target.value)}
              spellCheck={false}
            />
            {inputError && <p className="text-xs text-red-200">{inputError}</p>}
            <div className="flex justify-end gap-2">
              <Button type="button" className="bg-white/10 hover:bg-white/15" onClick={onClose}>Cancel</Button>
              <Button type="button" onClick={onStart} disabled={isStarting}>
                {isStarting ? "Running..." : "Run flow"}
              </Button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}

function ExecutionList({
  executions,
  selectedExecutionId,
  onSelectExecution,
}: {
  executions: FlowExecution[];
  selectedExecutionId: string;
  onSelectExecution: (execution: FlowExecution) => void;
}) {
  const [query, setQuery] = useState("");
  const filteredExecutions = executions.filter((execution) => executionMatches(execution, query));

  return (
    <div className="rounded-lg border border-white/10 bg-[#111214]">
      <div className="flex items-center justify-between border-b border-white/10 px-4 py-3">
        <h3 className="text-sm font-semibold text-white">Previous executions</h3>
        <Badge tone="slate">{filteredExecutions.length}</Badge>
      </div>
      <div className="border-b border-white/10 p-3">
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search id, status, step, action"
        />
        <div className="mt-2 flex flex-wrap gap-1.5">
          {["id", "status", "step", "action"].map((field) => (
            <span
              key={field}
              className="rounded-md border border-white/10 bg-white/[0.03] px-2 py-0.5 text-[11px] font-medium text-slate-500"
            >
              {field}
            </span>
          ))}
        </div>
      </div>
      {executions.length === 0 ? (
        <p className="p-4 text-sm text-slate-400">No flow executions yet.</p>
      ) : filteredExecutions.length === 0 ? (
        <p className="p-4 text-sm text-slate-400">No executions match this search.</p>
      ) : (
        <div className="divide-y divide-white/10">
          {filteredExecutions.map((execution) => (
            <button
              key={execution.id}
              type="button"
              onClick={() => onSelectExecution(execution)}
              className={cn(
                "grid w-full gap-1 px-4 py-3 text-left transition hover:bg-white/[0.04]",
                execution.id === selectedExecutionId && "bg-[#17181c] shadow-[inset_2px_0_0_#6f3dff]",
              )}
            >
              <span className="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] items-center gap-2">
                <code className="min-w-0 truncate text-xs text-slate-300">{execution.id}</code>
                <span className="min-w-0 max-w-full overflow-hidden">
                  <StatusBadge status={execution.status} />
                </span>
              </span>
              <span className="text-xs text-slate-500">{execution.steps.length} step(s)</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function executionMatches(execution: FlowExecution, query: string) {
  const value = query.trim().toLowerCase();
  if (!value) {
    return true;
  }

  return [
    execution.id,
    execution.status,
    ...execution.steps.map((step) => `${step.key} ${step.action} ${step.status}`),
  ].some((field) => field.toLowerCase().includes(value));
}

function ExecutionSummary({
  execution,
  isCancelling,
  onCancel,
}: {
  execution: FlowExecution;
  isCancelling: boolean;
  onCancel: () => void;
}) {
  const canCancel = isActiveExecution(execution);

  return (
    <div className="grid min-w-0 gap-4 rounded-lg border border-white/10 bg-[#111214] p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="text-sm font-semibold text-white">Flow execution details</h3>
          <code className="mt-1 block truncate text-xs text-slate-400">{execution.id}</code>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {canCancel && (
            <Button
              type="button"
              className="min-h-8 bg-red-500/90 px-2.5 hover:bg-red-400"
              disabled={isCancelling}
              onClick={onCancel}
            >
              {isCancelling ? "Cancelling..." : "Cancel"}
            </Button>
          )}
          <StatusBadge status={execution.status} />
        </div>
      </div>
      <dl className="grid gap-3 text-xs sm:grid-cols-4">
        <div>
          <dt className="text-slate-500">Flow input</dt>
          <dd className="mt-1 truncate text-slate-300">{shortJson(execution.input)}</dd>
        </div>
        <div>
          <dt className="text-slate-500">Flow output</dt>
          <dd className="mt-1 truncate text-slate-300">{shortJson(execution.output)}</dd>
        </div>
        <div>
          <dt className="text-slate-500">Steps</dt>
          <dd className="mt-1 text-slate-300">{execution.steps.length}</dd>
        </div>
        <div>
          <dt className="text-slate-500">Errors</dt>
          <dd className="mt-1 text-slate-300">{execution.steps.filter((step) => step.status === "failed").length}</dd>
        </div>
      </dl>
    </div>
  );
}

function ExecutionDetail({
  execution,
  selectedStep,
  onSelectStep,
  isRetryingStep,
  onRetryStep,
}: {
  execution: FlowExecution;
  selectedStep?: StepExecution;
  onSelectStep: (key: string) => void;
  isRetryingStep: boolean;
  onRetryStep: () => void;
}) {
  const canRetry = selectedStep ? canRetryStep(execution, selectedStep) : false;

  return (
    <div className="grid min-w-0 gap-4 rounded-lg border border-white/10 bg-[#111214] p-4">
      <h3 className="text-sm font-semibold text-white">Selected step details</h3>
      <DetailSection title="Input" value={execution.input} />
      <DetailSection title="Output" value={execution.output} />
      {selectedStep && (
        <div
          className={cn(
            "grid min-w-0 gap-3 rounded-md border bg-[#0b0c0e] p-3",
            isTimeoutStep(selectedStep) ? "border-amber-300/45" : "border-white/10",
          )}
        >
          <div className="flex items-center justify-between gap-3">
            <h4 className="min-w-0 truncate text-sm font-semibold text-white">{selectedStep.key}</h4>
            <div className="flex shrink-0 items-center gap-2">
              {canRetry && (
                <Button
                  type="button"
                  className="min-h-8 px-2.5"
                  disabled={isRetryingStep}
                  onClick={onRetryStep}
                >
                  {isRetryingStep ? "Retrying..." : "Retry step"}
                </Button>
              )}
              <StatusBadge status={selectedStep.status} />
            </div>
          </div>
          <DetailSection title="Step input" value={selectedStep.input} />
          <DetailSection title="Step output" value={selectedStep.output} />
          {selectedStep.error && <DetailSection title="Step error" value={selectedStep.error} />}
        </div>
      )}
      <details className="rounded-md border border-white/10 bg-[#0b0c0e] p-3">
        <summary className="cursor-pointer text-xs font-semibold uppercase text-slate-500">
          Timeline
        </summary>
        <div className="mt-3 grid gap-2">
          {execution.steps.map((step, index) => (
            <button
              key={`${step.key}-${index}`}
              type="button"
              onClick={() => onSelectStep(step.key)}
              className={cn(
                "flex min-w-0 items-center justify-between gap-3 rounded-md border border-white/10 bg-[#050506] px-3 py-2 text-left transition hover:border-white/15 hover:bg-white/[0.045]",
                selectedStep?.key === step.key && "border-violet-400/45 bg-[#17181c]",
              )}
            >
              <span className="truncate text-sm text-slate-300">{step.key}</span>
              <StatusBadge status={step.status} />
            </button>
          ))}
        </div>
      </details>
    </div>
  );
}

function DetailSection({ title, value }: { title: string; value: unknown }) {
  return (
    <div className="min-w-0">
      <h4 className="mb-2 text-xs font-semibold uppercase text-slate-500">{title}</h4>
      <CodeBlock>{typeof value === "string" ? value : JSON.stringify(value ?? null, null, 2)}</CodeBlock>
    </div>
  );
}

function shortJson(value: unknown): string {
  const text = typeof value === "string" ? value : JSON.stringify(value ?? null);
  return text.length > 80 ? `${text.slice(0, 77)}...` : text;
}

function WatchLog({ step }: { step?: StepExecution }) {
  return (
    <div className="grid min-w-0 gap-3 rounded-lg border border-white/10 bg-[#111214] p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="text-sm font-semibold text-white">Watch log</h3>
          <p className="mt-1 truncate text-xs text-slate-500">{step?.key ?? "No active step"}</p>
        </div>
        {step && <StatusBadge status={step.status} />}
      </div>
      <div className="h-[560px] overflow-auto rounded-md border border-white/10 bg-[#050506] p-3 font-mono text-xs leading-5">
        {!step ? (
          <p className="text-slate-500">Start an execution to watch step output.</p>
        ) : step.logs.length === 0 ? (
          <p className="text-slate-500">No logs were emitted for this step.</p>
        ) : (
          <div className="grid gap-2">
            {step.logs.map((log, index) => (
              <div key={`${step.key}-${index}`} className="rounded-md border border-white/10 bg-[#111214] p-2">
                <div className="flex items-center gap-2">
                  <span className="text-[11px] font-semibold uppercase text-violet-200">{log.level}</span>
                  <span className="min-w-0 truncate text-slate-200">{log.message}</span>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function StatusBadge({ status }: { status: ExecutionStatus }) {
  const tone =
    status === "succeeded"
      ? "green"
      : status === "failed"
        ? "red"
        : status === "running" || status === "queued"
          ? "violet"
          : "slate";
  return <Badge tone={tone}>{status}</Badge>;
}

function TimeoutBadge() {
  return <Badge tone="amber">timeout</Badge>;
}

function flowToGraph(
  flow: FlowDefinition,
  execution: FlowExecution | undefined,
  selectedStepKey: string,
  onSelectStep: (key: string) => void,
): { nodes: Node[]; edges: Edge[] } {
  const stepStates = new Map(execution?.steps.map((step) => [step.key, step]));
  const activeEdges = executionEdges(execution);
  const positions = layoutSteps(flow.steps);

  return {
    nodes: flow.steps.map((step) => ({
      id: step.key,
      position: positions.get(step.key) ?? { x: 0, y: 0 },
      data: {
        step,
        execution: stepStates.get(step.key),
        selected: step.key === selectedStepKey,
        onSelectStep,
      },
      type: "step",
    })),
    edges: flow.steps.flatMap((step) => stepEdges(step, activeEdges)),
  };
}

function executionEdges(execution: FlowExecution | undefined): Set<string> {
  const edges = new Set<string>();
  for (let index = 1; index < (execution?.steps.length ?? 0); index += 1) {
    edges.add(`${execution!.steps[index - 1].key}->${execution!.steps[index].key}`);
  }

  return edges;
}

function layoutSteps(steps: FlowStep[]): Map<string, { x: number; y: number }> {
  const stepByKey = new Map(steps.map((step) => [step.key, step]));
  const positions = new Map<string, { x: number; y: number }>();
  const occupied = new Set<string>();
  const xGap = 280;
  const yGap = 145;

  function openRow(column: number, row: number) {
    while (occupied.has(`${column}:${row}`)) {
      row += 1;
    }

    return row;
  }

  function place(key: string | undefined, column: number, row: number) {
    if (!key || positions.has(key)) {
      return;
    }

    const step = stepByKey.get(key);
    if (!step) {
      return;
    }

    row = openRow(column, row);
    occupied.add(`${column}:${row}`);
    positions.set(key, { x: column * xGap, y: row * yGap });

    const primary = primaryTarget(step);
    place(primary, column, row + 1);

    const branches = branchTargets(step).filter((branch) => branch.target !== primary);
    uniqueBy(branches, (branch) => branch.target).forEach((branch, index) => {
      const branchColumn =
        branch.label === "error"
          ? column + 4
          : column + (index % 2 === 0 ? 1 : -1) * (Math.floor(index / 2) + 2);
      place(branch.target, branchColumn, row + (branch.label === "error" ? 3 : 2));
    });
  }

  place(steps[0]?.key, 0, 0);

  for (const step of steps) {
    place(step.key, 0, positions.size);
  }

  return positions;
}

function primaryTarget(step: FlowStep): string | undefined {
  return step.next ?? step.next_when?.[0]?.next ?? step.otherwise;
}

function stepTargets(step: FlowStep): string[] {
  return [
    step.next,
    ...(step.next_when ?? []).map((branch) => branch.next),
    step.otherwise,
    step.on_error,
  ].filter((target): target is string => Boolean(target));
}

function unique<T>(values: T[]): T[] {
  return [...new Set(values)];
}

function branchTargets(step: FlowStep): Array<{ target: string; label: string }> {
  return [
    ...(step.next_when ?? []).map((branch) => ({ target: branch.next, label: "success" })),
    ...(step.otherwise ? [{ target: step.otherwise, label: "otherwise" }] : []),
    ...(step.on_error ? [{ target: step.on_error, label: "error" }] : []),
  ];
}

function uniqueBy<T>(values: T[], key: (value: T) => string): T[] {
  const seen = new Set<string>();
  return values.filter((value) => {
    const id = key(value);
    if (seen.has(id)) {
      return false;
    }
    seen.add(id);
    return true;
  });
}

function stepEdges(step: FlowStep, activeEdges: Set<string>): Edge[] {
  const edges: Edge[] = [];

  if (step.next) {
    edges.push(flowEdge(step.key, step.next, "next", activeEdges.has(`${step.key}->${step.next}`)));
  }

  for (const branch of step.next_when ?? []) {
    edges.push(flowEdge(step.key, branch.next, "success", activeEdges.has(`${step.key}->${branch.next}`)));
  }

  if (step.otherwise) {
    edges.push(flowEdge(step.key, step.otherwise, "otherwise", activeEdges.has(`${step.key}->${step.otherwise}`)));
  }

  if (step.on_error) {
    edges.push(flowEdge(step.key, step.on_error, "error", activeEdges.has(`${step.key}->${step.on_error}`)));
  }

  return edges;
}

function flowEdge(source: string, target: string, label: string, active: boolean): Edge {
  const color = active
    ? label === "error"
      ? "#f87171"
      : label === "success"
        ? "#34d399"
        : "#8b5cf6"
    : label === "error"
      ? "#7f1d1d"
      : label === "otherwise"
        ? "#a16207"
        : "#475569";

  return {
    id: `${source}-${target}-${label}`,
    source,
    target,
    sourceHandle: `${label}-source`,
    targetHandle: `${label}-target`,
    label,
    animated: active,
    type: "smoothstep",
    labelBgPadding: [8, 5],
    labelBgBorderRadius: 4,
    labelBgStyle: { fill: "#0b0c0e", fillOpacity: 0.98 },
    labelStyle: { fill: color, fontSize: 11, fontWeight: 800 },
    style: { stroke: color, strokeWidth: active ? 2.2 : 1.4 },
  };
}

const edgeHandles = [
  { label: "next", source: Position.Right, target: Position.Left, offset: "42%" },
  { label: "success", source: Position.Right, target: Position.Left, offset: "30%" },
  { label: "otherwise", source: Position.Right, target: Position.Left, offset: "58%" },
  { label: "error", source: Position.Bottom, target: Position.Bottom, offset: "78%" },
];

function handleStyle(position: Position, offset: string) {
  return position === Position.Left || position === Position.Right ? { top: offset } : { left: offset };
}

function StepNode({
  data,
}: NodeProps<
  Node<
    {
      step: FlowStep;
      execution?: StepExecution;
      selected: boolean;
      onSelectStep: (key: string) => void;
    },
    "step"
  >
>) {
  const step = data.step;
  const status = data.execution?.status;
  const timedOut = data.execution ? isTimeoutStep(data.execution) : false;
  const statusClass =
    status === "succeeded"
      ? "border-emerald-400/45"
      : status === "failed"
        ? timedOut
          ? "border-amber-300/60"
          : "border-red-400/45"
        : status === "running"
          ? "border-violet-400/55"
          : "border-white/10";

  return (
    <button
      type="button"
      onClick={() => data.onSelectStep(step.key)}
      className={cn(
        "min-w-[198px] rounded-md border bg-[#111214] p-3 text-left shadow-[0_1px_0_rgba(255,255,255,0.03)] transition hover:bg-[#17181c]",
        statusClass,
        data.selected && "ring-1 ring-violet-400/70",
      )}
    >
      {edgeHandles.map((handle) => (
        <Handle
          key={`${handle.label}-target`}
          id={`${handle.label}-target`}
          type="target"
          position={handle.target}
          style={handleStyle(handle.target, handle.offset)}
          className="!h-2 !w-2 !rounded-sm !border !border-[#111214] !bg-slate-500"
        />
      ))}
      <div className="mb-2 flex items-center gap-2">
        <span
          className={cn(
            "h-2 w-2 rounded-sm",
            status === "succeeded"
              ? "bg-emerald-400"
              : status === "failed"
                ? "bg-red-400"
                : status === "running"
                  ? "bg-violet-400"
                  : "bg-slate-500",
          )}
        />
        <strong className="truncate text-sm font-semibold text-white">{step.key}</strong>
        {status && (
          <span className="ml-auto flex items-center gap-1">
            {timedOut && <TimeoutBadge />}
            <StatusBadge status={status} />
          </span>
        )}
      </div>
      <code className="block truncate rounded-sm border border-white/10 bg-[#050506] px-2 py-1 text-xs text-slate-300">
        {step.action}
      </code>
      {edgeHandles.map((handle) => (
        <Handle
          key={`${handle.label}-source`}
          id={`${handle.label}-source`}
          type="source"
          position={handle.source}
          style={handleStyle(handle.source, handle.offset)}
          className="!h-2 !w-2 !rounded-sm !border !border-[#111214] !bg-violet-400"
        />
      ))}
    </button>
  );
}

async function startFlowExecution(flowKey: string, input: unknown): Promise<FlowExecution> {
  const started = await requestJson<StartFlowResponse>(`/internal/flows/${encodeURIComponent(flowKey)}/runs`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(input ?? {}),
  });

  return toFlowExecution({
    id: started.id,
    flow_key: started.flow_key,
    status: started.status,
    input,
    output: null,
    error: null,
    steps: [],
  });
}

async function cancelFlowExecution(id: string): Promise<FlowExecution> {
  return toFlowExecution(await requestJson<RawFlowExecution>(`/internal/flows/runs/${encodeURIComponent(id)}/cancel`, {
    method: "POST",
  }));
}

async function retryFlowStep(id: string, stepKey: string): Promise<FlowExecution> {
  return toFlowExecution(await requestJson<RawFlowExecution>(
    `/internal/flows/runs/${encodeURIComponent(id)}/steps/${encodeURIComponent(stepKey)}/retry`,
    { method: "POST" },
  ));
}

async function pollFlowExecution(id: string, onUpdate: (execution: FlowExecution) => void): Promise<void> {
  const deadline = Date.now() + 10_000;

  while (true) {
    const execution = toFlowExecution(await requestJson<RawFlowExecution>(`/internal/flows/runs/${encodeURIComponent(id)}`));
    onUpdate(execution);

    if (execution.status !== "queued" && execution.status !== "running") {
      return;
    }

    if (Date.now() > deadline) {
      return;
    }

    await sleep(250);
  }
}

async function requestJson<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, init);
  const body = await response.json();

  if (!response.ok) {
    throw new Error(body?.message ?? body?.error ?? "Request failed.");
  }

  return body as T;
}

function toFlowExecution(execution: RawFlowExecution): FlowExecution {
  return {
    id: execution.id,
    flow_key: execution.flow_key,
    status: normalizeStatus(execution.status),
    input: execution.input ?? null,
    output: execution.output ?? null,
    error: execution.error ?? null,
    steps: (execution.steps ?? []).map((step) => ({
      key: step.key,
      action: step.action,
      status: normalizeStatus(step.status),
      input: step.input ?? null,
      output: step.output ?? null,
      error: step.error ?? null,
      invocation_id: step.invocation_id ?? null,
      logs: step.logs ?? [],
    })),
  };
}

function normalizeStatus(status: string): ExecutionStatus {
  return status === "queued" ||
    status === "running" ||
    status === "succeeded" ||
    status === "failed" ||
    status === "skipped" ||
    status === "cancelled"
    ? status
    : "failed";
}

function isActiveExecution(execution: FlowExecution): boolean {
  return execution.status === "queued" || execution.status === "running";
}

function canRetryStep(execution: FlowExecution, step: StepExecution): boolean {
  return execution.status === "failed" && step.status === "failed";
}

function isTimeoutStep(step: StepExecution): boolean {
  return step.status === "failed" && (step.error ?? "").toLowerCase().includes("timed out");
}

function sleep(ms: number) {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

function upsertExecution(executions: FlowExecution[], execution: FlowExecution) {
  const index = executions.findIndex((current) => current.id === execution.id);

  if (index === -1) {
    return [execution, ...executions];
  }

  return executions.map((current) => current.id === execution.id ? execution : current);
}

type StartFlowResponse = {
  id: string;
  flow_key: string;
  status: string;
};

type RawFlowExecution = {
  id: string;
  flow_key: string;
  status: string;
  input?: unknown;
  output?: unknown;
  error?: string | null;
  steps?: Array<{
    key: string;
    action: string;
    status: string;
    invocation_id?: string | null;
    input?: unknown;
    output?: unknown;
    error?: string | null;
    logs?: FlowStepLog[];
  }>;
};
