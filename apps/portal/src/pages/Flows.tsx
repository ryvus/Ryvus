import {
  Background,
  Controls,
  MiniMap,
  ReactFlow,
  type Edge,
  type Node,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useMemo } from "react";
import type { Artifacts, FlowDefinition, FlowStep } from "../artifacts/types";

export function Flows({ artifacts }: { artifacts: Artifacts }) {
  const flows = artifacts.flows.flows;

  return (
    <div className="page">
      <div className="section-heading">
        <span className="eyebrow">FlowSpec</span>
        <h1>Flows</h1>
      </div>
      {flows.length === 0 ? (
        <p>No flows were found in this artifact snapshot.</p>
      ) : (
        <div className="flow-list">
          {flows.map((flow) => (
            <FlowCard key={flow.key} flow={flow} />
          ))}
        </div>
      )}
    </div>
  );
}

function FlowCard({ flow }: { flow: FlowDefinition }) {
  const graph = useMemo(() => flowToGraph(flow), [flow]);

  return (
    <section className="flow-card">
      <div className="flow-header">
        <div>
          <h2>{flow.key}</h2>
          {flow.description && <p>{flow.description}</p>}
        </div>
        {flow.version && <span className="status-pill">v{flow.version}</span>}
      </div>
      <div className="flow-diagram">
        <ReactFlow
          nodes={graph.nodes}
          edges={graph.edges}
          fitView
          nodesDraggable={false}
          nodesConnectable={false}
          elementsSelectable={false}
        >
          <Background color="#ddd8cf" gap={18} />
          <MiniMap pannable zoomable />
          <Controls showInteractive={false} />
        </ReactFlow>
      </div>
      <details className="flow-source">
        <summary>Source</summary>
        <pre>{JSON.stringify(flow, null, 2)}</pre>
      </details>
    </section>
  );
}

function flowToGraph(flow: FlowDefinition): { nodes: Node[]; edges: Edge[] } {
  return {
    nodes: flow.steps.map((step, index) => ({
      id: step.key,
      position: { x: index * 260, y: 0 },
      data: { label: <StepLabel step={step} /> },
      type: "default",
    })),
    edges: flow.steps.flatMap(stepEdges),
  };
}

function stepEdges(step: FlowStep): Edge[] {
  const edges: Edge[] = [];

  if (step.next) {
    edges.push(flowEdge(step.key, step.next, "next"));
  }

  for (const branch of step.next_when ?? []) {
    edges.push(flowEdge(step.key, branch.next, branch.when));
  }

  if (step.otherwise) {
    edges.push(flowEdge(step.key, step.otherwise, "otherwise"));
  }

  if (step.on_error) {
    edges.push(flowEdge(step.key, step.on_error, "on_error"));
  }

  return edges;
}

function flowEdge(source: string, target: string, label: string): Edge {
  return {
    id: `${source}-${target}-${label}`,
    source,
    target,
    label,
    animated: label === "on_error",
    type: "smoothstep",
  };
}

function StepLabel({ step }: { step: FlowStep }) {
  return (
    <div className="flow-node-label">
      <strong>{step.key}</strong>
      <code>{step.action}</code>
    </div>
  );
}
