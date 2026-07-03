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

type ExecutionStatus = "running" | "succeeded" | "failed" | "canceled";

type NodeExecution = {
  key: string;
  action: string;
  status: ExecutionStatus;
  input: unknown;
  output: unknown;
  error: string | null;
  started_at: string;
  finished_at: string | null;
};

type FlowExecution = {
  id: string;
  flow_key: string;
  status: ExecutionStatus;
  input: unknown;
  output: unknown;
  error: string | null;
  started_at: string;
  finished_at: string | null;
  nodes: NodeExecution[];
};

const nodeTypes = {
  step: StepNode,
};

export function Flows({ artifacts }: { artifacts: Artifacts }) {
  const flows = artifacts.flows.flows;
  const [selectedFlowKey, setSelectedFlowKey] = useState("");
  const [executions, setExecutions] = useState<FlowExecution[]>([]);
  const [selectedExecutionId, setSelectedExecutionId] = useState("");
  const [selectedNodeKey, setSelectedNodeKey] = useState("");
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
          selectedNodeKey={selectedNodeKey}
          onSelectExecution={(execution) => {
            setSelectedExecutionId(execution.id);
            setSelectedNodeKey(execution.nodes[0]?.key ?? "");
          }}
          onSelectNode={setSelectedNodeKey}
          onStartExecution={(input) => {
            const execution = createMockExecution(selectedFlow, input);
            setExecutions((current) => [execution, ...current]);
            setSelectedExecutionId(execution.id);
            setSelectedNodeKey(execution.nodes[0]?.key ?? "");
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
  selectedNodeKey,
  onSelectExecution,
  onSelectNode,
  onStartExecution,
}: {
  flow: FlowDefinition;
  executions: FlowExecution[];
  selectedExecutionId: string;
  selectedNodeKey: string;
  onSelectExecution: (execution: FlowExecution) => void;
  onSelectNode: (key: string) => void;
  onStartExecution: (input: unknown) => void;
}) {
  const [inputText, setInputText] = useState("{\n  \"input\": true\n}");
  const [inputError, setInputError] = useState("");
  const [isStartOpen, setIsStartOpen] = useState(false);
  const [activeView, setActiveView] = useState<"diagram" | "source">("diagram");
  const selectedExecution =
    executions.find((execution) => execution.id === selectedExecutionId) ?? executions[0];
  const selectedNode =
    selectedExecution?.nodes.find((node) => node.key === selectedNodeKey) ??
    selectedExecution?.nodes[0];
  const graph = useMemo(
    () => flowToGraph(flow, selectedExecution, selectedNode?.key ?? selectedNodeKey, onSelectNode),
    [flow, selectedExecution, selectedNode?.key, selectedNodeKey, onSelectNode],
  );

  function startExecution() {
    try {
      const input = inputText.trim() ? JSON.parse(inputText) : {};
      setInputError("");
      onStartExecution(input);
      return true;
    } catch (error) {
      setInputError(error instanceof Error ? error.message : "Invalid JSON input.");
      return false;
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
            onStart={() => {
              if (startExecution()) {
                setIsStartOpen(false);
              }
            }}
          />
          {selectedExecution && <ExecutionSummary execution={selectedExecution} />}
          <div className="overflow-hidden rounded-xl border border-white/10 bg-slate-950/80">
            <div className="flex items-center gap-1 border-b border-white/10 bg-black/20 p-2">
              {(["diagram", "source"] as const).map((view) => (
                <button
                  key={view}
                  type="button"
                  onClick={() => setActiveView(view)}
                  className={cn(
                    "rounded-lg px-3 py-1.5 text-sm font-medium capitalize text-slate-400 transition hover:bg-white/5 hover:text-white",
                    activeView === view && "bg-white/10 text-white",
                  )}
                >
                  {view}
                </button>
              ))}
            </div>
            {activeView === "diagram" ? (
              <div className="ryvus-flow h-[620px]">
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
                  <Background color="rgba(148, 163, 184, 0.22)" gap={18} />
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
                  selectedNode={selectedNode}
                  onSelectNode={onSelectNode}
                />
              </div>
              <WatchLog node={selectedNode} />
            </div>
          ) : (
            <EmptyState
              title="No executions"
              message="Start a flow execution to inspect input, output, node state, and timeline."
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
  onStart,
}: {
  inputText: string;
  inputError: string;
  isOpen: boolean;
  onOpen: () => void;
  onClose: () => void;
  onInputTextChange: (value: string) => void;
  onStart: () => void;
}) {
  return (
    <>
      <div className="flex items-center justify-between gap-3 rounded-xl border border-white/10 bg-black/20 p-4">
        <div>
          <h3 className="text-sm font-semibold text-white">Start new execution</h3>
          <p className="mt-1 text-xs leading-5 text-slate-400">Open the input editor and start a run.</p>
        </div>
        <Button type="button" onClick={onOpen}>Start execution</Button>
      </div>
      {isOpen && (
        <div className="fixed inset-0 z-50 grid items-start justify-items-center overflow-auto bg-slate-950/76 p-4 pt-8 backdrop-blur-sm sm:pt-12">
          <div className="grid w-full max-w-3xl gap-4 rounded-xl border border-white/10 bg-slate-950 p-5 shadow-[0_24px_80px_rgba(0,0,0,0.55)]">
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
              <Button type="button" onClick={onStart}>Run flow</Button>
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
    <div className="rounded-xl border border-white/10 bg-black/20">
      <div className="flex items-center justify-between border-b border-white/10 px-4 py-3">
        <h3 className="text-sm font-semibold text-white">Previous executions</h3>
        <Badge tone="slate">{filteredExecutions.length}</Badge>
      </div>
      <div className="border-b border-white/10 p-3">
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search id, status, date, node, action"
        />
        <div className="mt-2 flex flex-wrap gap-1.5">
          {["id", "status", "date/time", "node", "action"].map((field) => (
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
                execution.id === selectedExecutionId && "bg-blue-500/10",
              )}
            >
              <span className="flex items-center justify-between gap-3">
                <code className="truncate text-xs text-slate-300">{execution.id}</code>
                <StatusBadge status={execution.status} />
              </span>
              <span className="text-xs text-slate-500">
                {new Date(execution.started_at).toLocaleString()}
              </span>
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
    new Date(execution.started_at).toLocaleString(),
    ...execution.nodes.map((node) => `${node.key} ${node.action} ${node.status}`),
  ].some((field) => field.toLowerCase().includes(value));
}

function ExecutionSummary({ execution }: { execution: FlowExecution }) {
  return (
    <div className="grid min-w-0 gap-4 rounded-xl border border-white/10 bg-black/20 p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="text-sm font-semibold text-white">Flow execution details</h3>
          <code className="mt-1 block truncate text-xs text-slate-400">{execution.id}</code>
        </div>
        <StatusBadge status={execution.status} />
      </div>
      <dl className="grid gap-3 text-xs sm:grid-cols-4">
        <div>
          <dt className="text-slate-500">Started</dt>
          <dd className="mt-1 text-slate-300">{new Date(execution.started_at).toLocaleTimeString()}</dd>
        </div>
        <div>
          <dt className="text-slate-500">Duration</dt>
          <dd className="mt-1 text-slate-300">{executionDuration(execution)}</dd>
        </div>
        <div>
          <dt className="text-slate-500">Nodes</dt>
          <dd className="mt-1 text-slate-300">{execution.nodes.length}</dd>
        </div>
        <div>
          <dt className="text-slate-500">Errors</dt>
          <dd className="mt-1 text-slate-300">{execution.nodes.filter((node) => node.status === "failed").length}</dd>
        </div>
      </dl>
    </div>
  );
}

function ExecutionDetail({
  execution,
  selectedNode,
  onSelectNode,
}: {
  execution: FlowExecution;
  selectedNode?: NodeExecution;
  onSelectNode: (key: string) => void;
}) {
  return (
    <div className="grid min-w-0 gap-4 rounded-xl border border-white/10 bg-black/20 p-4">
      <h3 className="text-sm font-semibold text-white">Selected node details</h3>
      <DetailSection title="Input" value={execution.input} />
      <DetailSection title="Output" value={execution.output} />
      {selectedNode && (
        <div className="grid min-w-0 gap-3 rounded-lg border border-white/10 bg-slate-950/70 p-3">
          <div className="flex items-center justify-between gap-3">
            <h4 className="min-w-0 truncate text-sm font-semibold text-white">{selectedNode.key}</h4>
            <StatusBadge status={selectedNode.status} />
          </div>
          <DetailSection title="Node input" value={selectedNode.input} />
          <DetailSection title="Node output" value={selectedNode.output} />
          {selectedNode.error && <DetailSection title="Node error" value={selectedNode.error} />}
        </div>
      )}
      <details className="rounded-lg border border-white/10 bg-slate-950/45 p-3">
        <summary className="cursor-pointer text-xs font-semibold uppercase text-slate-500">
          Timeline
        </summary>
        <div className="mt-3 grid gap-2">
          {execution.nodes.map((node) => (
            <button
              key={node.key}
              type="button"
              onClick={() => onSelectNode(node.key)}
              className={cn(
                "flex min-w-0 items-center justify-between gap-3 rounded-lg border border-white/10 bg-slate-950/55 px-3 py-2 text-left transition hover:border-blue-400/35 hover:bg-blue-500/10",
                selectedNode?.key === node.key && "border-blue-400/45 bg-blue-500/10",
              )}
            >
              <span className="truncate text-sm text-slate-300">{node.key}</span>
              <StatusBadge status={node.status} />
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

function WatchLog({ node }: { node?: NodeExecution }) {
  const lines = node ? fakeLogsForNode(node) : [];

  return (
    <div className="grid min-w-0 gap-3 rounded-xl border border-white/10 bg-black/20 p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="text-sm font-semibold text-white">Watch log</h3>
          <p className="mt-1 truncate text-xs text-slate-500">{node?.key ?? "No active node"}</p>
        </div>
        {node && <StatusBadge status={node.status} />}
      </div>
      <div className="h-[560px] overflow-auto rounded-lg border border-white/10 bg-slate-950/80 p-3 font-mono text-xs leading-5">
        {lines.length === 0 ? (
          <p className="text-slate-500">Start an execution to watch node output.</p>
        ) : (
          lines.map((line) => (
            <div
              key={`${line.time}-${line.message}`}
              className={cn(
                "grid grid-cols-[54px_minmax(0,1fr)] gap-2",
                line.level === "error" ? "text-red-200" : line.level === "warn" ? "text-amber-200" : "text-slate-300",
              )}
            >
              <span className="text-slate-600">{line.time}</span>
              <span className="min-w-0 break-words">{line.message}</span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

function fakeLogsForNode(node: NodeExecution) {
  const base = [
    { time: "00:00", level: "info", message: `started ${node.action}` },
    { time: "00:01", level: "info", message: `input accepted for ${node.key}` },
  ];

  if (node.status === "running") {
    return [
      ...base,
      { time: "00:02", level: "info", message: "waiting for manual decision" },
      { time: "00:03", level: "warn", message: "node still active, streaming output" },
    ];
  }

  if (node.status === "failed") {
    return [
      ...base,
      { time: "00:02", level: "error", message: node.error ?? "node failed" },
      { time: "00:03", level: "warn", message: "failure path selected" },
    ];
  }

  return [
    ...base,
    { time: "00:02", level: "info", message: `output: ${JSON.stringify(node.output)}` },
    { time: "00:03", level: "info", message: "node completed successfully" },
  ];
}

function StatusBadge({ status }: { status: ExecutionStatus }) {
  const tone =
    status === "succeeded" ? "green" : status === "failed" ? "red" : status === "running" ? "blue" : "slate";
  return <Badge tone={tone}>{status}</Badge>;
}

function flowToGraph(
  flow: FlowDefinition,
  execution: FlowExecution | undefined,
  selectedNodeKey: string,
  onSelectNode: (key: string) => void,
): { nodes: Node[]; edges: Edge[] } {
  const nodeStates = new Map(execution?.nodes.map((node) => [node.key, node]));
  const positions = layoutSteps(flow.steps);

  return {
    nodes: flow.steps.map((step) => ({
      id: step.key,
      position: positions.get(step.key) ?? { x: 0, y: 0 },
      data: {
        step,
        execution: nodeStates.get(step.key),
        selected: step.key === selectedNodeKey,
        onSelectNode,
      },
      type: "step",
    })),
    edges: flow.steps.flatMap(stepEdges),
  };
}

function layoutSteps(steps: FlowStep[]): Map<string, { x: number; y: number }> {
  const levels = new Map(steps.map((step) => [step.key, 0]));

  for (let pass = 0; pass < steps.length; pass += 1) {
    for (const step of steps) {
      const level = levels.get(step.key) ?? 0;
      for (const target of stepTargets(step)) {
        levels.set(target, Math.max(levels.get(target) ?? 0, level + 1));
      }
    }
  }

  const columns = new Map<number, FlowStep[]>();
  for (const step of steps) {
    const level = levels.get(step.key) ?? 0;
    columns.set(level, [...(columns.get(level) ?? []), step]);
  }

  const positions = new Map<string, { x: number; y: number }>();
  for (const [level, column] of columns) {
    column.forEach((step, index) => {
      positions.set(step.key, {
        x: level * 330,
        y: (index - (column.length - 1) / 2) * 190,
      });
    });
  }

  return positions;
}

function stepTargets(step: FlowStep): string[] {
  return [
    step.next,
    ...(step.next_when ?? []).map((branch) => branch.next),
    step.otherwise,
    step.on_error,
  ].filter((target): target is string => Boolean(target));
}

function stepEdges(step: FlowStep): Edge[] {
  const edges: Edge[] = [];

  if (step.next) {
    edges.push(flowEdge(step.key, step.next, "next"));
  }

  for (const branch of step.next_when ?? []) {
    edges.push(flowEdge(step.key, branch.next, "success"));
  }

  if (step.otherwise) {
    edges.push(flowEdge(step.key, step.otherwise, "otherwise"));
  }

  if (step.on_error) {
    edges.push(flowEdge(step.key, step.on_error, "error"));
  }

  return edges;
}

function flowEdge(source: string, target: string, label: string): Edge {
  return {
    id: `${source}-${target}-${label}`,
    source,
    target,
    label,
    animated: label === "error",
    type: "smoothstep",
    labelBgPadding: [8, 5],
    labelBgBorderRadius: 6,
    labelBgStyle: { fill: "#0f172a", fillOpacity: 0.96 },
    labelStyle: { fill: "#e2e8f0", fontSize: 12, fontWeight: 700 },
  };
}

function StepNode({
  data,
}: NodeProps<
  Node<
    {
      step: FlowStep;
      execution?: NodeExecution;
      selected: boolean;
      onSelectNode: (key: string) => void;
    },
    "step"
  >
>) {
  const step = data.step;
  const status = data.execution?.status;
  const statusClass =
    status === "succeeded"
      ? "border-emerald-400/35"
      : status === "failed"
        ? "border-red-400/35"
        : status === "running"
          ? "border-blue-400/45"
          : "border-blue-400/20";

  return (
    <button
      type="button"
      onClick={() => data.onSelectNode(step.key)}
      className={cn(
        "min-w-[190px] rounded-xl border bg-slate-950/95 p-3 text-left shadow-[0_18px_50px_rgba(2,6,23,0.38)] transition hover:bg-slate-900",
        statusClass,
        data.selected && "ring-2 ring-blue-400/50",
      )}
    >
      <Handle type="target" position={Position.Left} className="!h-2.5 !w-2.5 !border-2 !border-slate-950 !bg-blue-400" />
      <div className="mb-2 flex items-center gap-2">
        <span className="h-2 w-2 rounded-full bg-gradient-to-br from-blue-400 to-violet-500 shadow-[0_0_0_4px_rgba(37,99,255,0.14)]" />
        <strong className="truncate text-sm font-semibold text-white">{step.key}</strong>
        {status && <span className="ml-auto"><StatusBadge status={status} /></span>}
      </div>
      <code className="block truncate rounded-md border border-white/10 bg-black/25 px-2 py-1 text-xs text-slate-300">
        {step.action}
      </code>
      <Handle type="source" position={Position.Right} className="!h-2.5 !w-2.5 !border-2 !border-slate-950 !bg-violet-400" />
    </button>
  );
}

function createMockExecution(flow: FlowDefinition, input: unknown): FlowExecution {
  const startedAt = new Date();
  const nodes = flow.steps.map((step, index) => {
    const nodeStarted = new Date(startedAt.getTime() + index * 120);
    const nodeFinished = new Date(nodeStarted.getTime() + 80);
    const status = mockNodeStatus(step);
    return {
      key: step.key,
      action: step.action,
      status,
      input: mockNodeInput(step, input, flow.steps[index - 1]?.key),
      output: status === "failed" ? null : mockNodeOutput(step, status),
      error: status === "failed" ? mockNodeError(step) : null,
      started_at: nodeStarted.toISOString(),
      finished_at: status === "running" ? null : nodeFinished.toISOString(),
    };
  });
  const finishedAt = nodes.some((node) => node.status === "running")
    ? null
    : new Date(startedAt.getTime() + Math.max(nodes.length, 1) * 120);
  const failed = nodes.some((node) => node.status === "failed");
  const running = nodes.some((node) => node.status === "running");

  return {
    id: `flowexec_${Date.now().toString(36)}`,
    flow_key: flow.key,
    status: running ? "running" : failed ? "failed" : "succeeded",
    input,
    output: nodes.at(-1)?.output ?? null,
    error: failed ? "Billing workflow moved through failure handling." : null,
    started_at: startedAt.toISOString(),
    finished_at: finishedAt?.toISOString() ?? null,
    nodes,
  };
}

function mockNodeStatus(step: FlowStep): ExecutionStatus {
  if (
    step.key.includes("failure") ||
    step.key.includes("failed") ||
    step.key.includes("error") ||
    step.key.includes("collections")
  ) {
    return "failed";
  }
  if (step.key.includes("manual_review")) {
    return "running";
  }
  return "succeeded";
}

function mockNodeInput(step: FlowStep, flowInput: unknown, previousStep?: string) {
  return step.key.includes("receive")
    ? flowInput
    : { from: previousStep, action: step.action };
}

function mockNodeOutput(step: FlowStep, status: ExecutionStatus) {
  return {
    ok: status === "succeeded",
    status,
    action: step.action,
    step: step.key,
  };
}

function mockNodeError(step: FlowStep) {
  return `${step.action} returned a simulated billing error.`;
}

function executionDuration(execution: FlowExecution) {
  if (!execution.finished_at) {
    return "running";
  }

  return `${Math.max(
    0,
    new Date(execution.finished_at).getTime() - new Date(execution.started_at).getTime(),
  )}ms`;
}
