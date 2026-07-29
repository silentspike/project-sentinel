export type DeliveryStage =
  | "candidate"
  | "qa"
  | "release"
  | "delivery"
  | "acceptance"
  | "closeout"
  | "rollback";

export interface DeliveryLineageNode {
  id: string;
  stage: DeliveryStage;
  label: string;
  state: string;
  digest: string;
  generation: number;
  actorRole: string;
  costUsd?: number;
}

export interface DeliveryLineageEdge {
  from: string;
  to: string;
}

export interface DeliveryLineageSnapshot {
  schemaVersion: 1;
  tenantId: string;
  projectId: string;
  revision: number;
  nodes: DeliveryLineageNode[];
  edges: DeliveryLineageEdge[];
  blockers: string[];
  adapterReady: boolean;
  readAt: string;
}

const SHA256 = /^[a-f0-9]{64}$/;
const SENSITIVE_ASSIGNMENT = /\b(secret|token|password|api[_-]?key)\s*[:=]\s*[^\s,;]+/gi;
const INTERNAL_ADDRESS = /\b10\.(?:\d{1,3}\.){2}\d{1,3}\b/g;
const INTERNAL_PATH = /\/(?:home|work|tmp)\/[^\s,;]+/g;

export function validateLineage(snapshot: DeliveryLineageSnapshot): string[] {
  const failures: string[] = [];
  if (snapshot.schemaVersion !== 1) failures.push("unsupported schema");
  if (!snapshot.tenantId || !snapshot.projectId) failures.push("missing authority scope");
  if (!Number.isSafeInteger(snapshot.revision) || snapshot.revision < 0) {
    failures.push("invalid revision");
  }

  const ids = new Set<string>();
  for (const node of snapshot.nodes) {
    if (!node.id || ids.has(node.id)) failures.push(`duplicate or empty node: ${node.id}`);
    ids.add(node.id);
    if (!SHA256.test(node.digest)) failures.push(`invalid digest: ${node.id}`);
    if (!Number.isSafeInteger(node.generation) || node.generation < 1) {
      failures.push(`invalid generation: ${node.id}`);
    }
    if (!node.actorRole) failures.push(`missing authority role: ${node.id}`);
    if (node.costUsd !== undefined && (!Number.isFinite(node.costUsd) || node.costUsd < 0)) {
      failures.push(`invalid cost: ${node.id}`);
    }
  }
  for (const edge of snapshot.edges) {
    if (!ids.has(edge.from) || !ids.has(edge.to)) {
      failures.push(`dangling edge: ${edge.from}->${edge.to}`);
    }
  }
  return failures;
}

export function shortDigest(value: string): string {
  return SHA256.test(value) ? `${value.slice(0, 10)}...${value.slice(-6)}` : "invalid";
}

export function publicLineage(snapshot: DeliveryLineageSnapshot): DeliveryLineageSnapshot {
  return {
    ...snapshot,
    tenantId: "redacted",
    projectId: scrubPublicText(snapshot.projectId),
    nodes: snapshot.nodes.map((node) => ({
      ...node,
      label: scrubPublicText(node.label),
      state: scrubPublicText(node.state),
      actorRole: scrubPublicText(node.actorRole),
    })),
    blockers: snapshot.blockers.map(scrubPublicText),
  };
}

function scrubPublicText(value: string): string {
  return value
    .trim()
    .replace(SENSITIVE_ASSIGNMENT, "$1=[redacted]")
    .replace(INTERNAL_ADDRESS, "[internal-address]")
    .replace(INTERNAL_PATH, "[internal-path]");
}
