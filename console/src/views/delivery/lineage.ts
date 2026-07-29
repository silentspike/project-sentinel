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

/**
 * Public DTO for the isolated #696 scaffold.
 *
 * The future authenticated server adapter must construct this DTO after
 * tenant authorization and redaction. Raw aggregate snapshots, tenant IDs,
 * prompts, credentials, private artifact content and infrastructure
 * identifiers are deliberately not representable here.
 */
export interface PublicDeliveryLineageDto {
  schemaVersion: 1;
  serverRedacted: true;
  projectLabel: string;
  revision: number;
  nodes: DeliveryLineageNode[];
  edges: DeliveryLineageEdge[];
  blockers: string[];
  adapterReady: boolean;
  readAt: string;
}

const SHA256 = /^[a-f0-9]{64}$/;
const FORBIDDEN_PUBLIC_TEXT =
  /\b(secret|token|password|api[_-]?key)\s*[:=]|\b10\.(?:\d{1,3}\.){2}\d{1,3}\b|\/(?:home|work|tmp)\//i;

export function validateLineage(snapshot: PublicDeliveryLineageDto): string[] {
  const failures: string[] = [];
  if (snapshot.schemaVersion !== 1) failures.push("unsupported schema");
  if (snapshot.serverRedacted !== true || !snapshot.projectLabel) {
    failures.push("missing server-redacted DTO marker");
  }
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
  const publicText = [
    snapshot.projectLabel,
    ...snapshot.nodes.flatMap((node) => [node.label, node.state, node.actorRole]),
    ...snapshot.blockers,
  ];
  if (publicText.some((value) => FORBIDDEN_PUBLIC_TEXT.test(value))) {
    failures.push("forbidden sensitive or internal text");
  }
  return failures;
}

export function shortDigest(value: string): string {
  return SHA256.test(value) ? `${value.slice(0, 10)}...${value.slice(-6)}` : "invalid";
}
