export type DeliveryStage =
  | "customer_request"
  | "agreement"
  | "project"
  | "work_item"
  | "participant"
  | "decision"
  | "handoff"
  | "blocker"
  | "candidate"
  | "qa"
  | "workbench"
  | "artifact"
  | "review"
  | "test"
  | "finding"
  | "approval"
  | "manifest"
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
  costMinor?: string;
  currency?: string;
}

export interface DeliveryLineageEdge {
  from: string;
  to: string;
}

/**
 * Public DTO for the authenticated #696 lineage surface.
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
  authorityGeneration: number;
  readAtMs: number;
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
  if (
    !Number.isSafeInteger(snapshot.authorityGeneration) ||
    snapshot.authorityGeneration < 1 ||
    !Number.isSafeInteger(snapshot.readAtMs) ||
    snapshot.readAtMs < 0
  ) {
    failures.push("invalid authority or read generation");
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
    if (node.costMinor !== undefined) {
      if (!/^(?:0|[1-9][0-9]*)$/.test(node.costMinor) || node.currency !== "USD") {
        failures.push(`invalid cost: ${node.id}`);
      }
    } else if (node.currency !== undefined) {
      failures.push(`invalid cost: ${node.id}`);
    }
  }
  const edgeKeys = new Set<string>();
  const outgoing = new Map<string, string[]>();
  const indegree = new Map([...ids].map((id) => [id, 0]));
  for (const edge of snapshot.edges) {
    if (!ids.has(edge.from) || !ids.has(edge.to)) {
      failures.push(`dangling edge: ${edge.from}->${edge.to}`);
      continue;
    }
    const edgeKey = `${edge.from}\u0000${edge.to}`;
    if (edgeKeys.has(edgeKey)) {
      failures.push(`duplicate edge: ${edge.from}->${edge.to}`);
      continue;
    }
    edgeKeys.add(edgeKey);
    if (edge.from === edge.to) {
      failures.push(`self edge: ${edge.from}`);
      continue;
    }
    outgoing.set(edge.from, [...(outgoing.get(edge.from) ?? []), edge.to]);
    indegree.set(edge.to, (indegree.get(edge.to) ?? 0) + 1);
  }
  if (ids.size === 0) {
    failures.push("empty lineage graph");
  } else if (![...ids].some((id) => (indegree.get(id) ?? 0) === 0)) {
    failures.push("cyclic lineage graph");
  } else {
    const roots = [...ids].filter((id) => (indegree.get(id) ?? 0) === 0);
    if (roots.length !== 1) failures.push("disconnected lineage graph");
    const reachable = new Set<string>();
    const visiting = new Set<string>();
    const visit = (id: string): boolean => {
      if (visiting.has(id)) return false;
      if (reachable.has(id)) return true;
      visiting.add(id);
      for (const child of outgoing.get(id) ?? []) {
        if (!visit(child)) return false;
      }
      visiting.delete(id);
      reachable.add(id);
      return true;
    };
    if (!roots.every(visit)) failures.push("cyclic lineage graph");
    if (reachable.size !== ids.size) failures.push("disconnected lineage graph");
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

export function parsePublicDeliveryLineageDto(value: unknown): PublicDeliveryLineageDto {
  const root = exactObject(value, [
    "schemaVersion",
    "serverRedacted",
    "projectLabel",
    "revision",
    "nodes",
    "edges",
    "blockers",
    "adapterReady",
    "authorityGeneration",
    "readAtMs",
  ]);
  if (root.serverRedacted !== true) {
    throw new Error("delivery lineage is not server-redacted");
  }
  if (typeof root.adapterReady !== "boolean") {
    throw new Error("adapterReady is not a boolean");
  }
  const nodes = arrayValue(root.nodes, "nodes").map((entry) => {
    const node = exactObject(
      entry,
      ["id", "stage", "label", "state", "digest", "generation", "actorRole"],
      ["costMinor", "currency"],
    );
    return {
      id: stringValue(node.id, "node.id"),
      stage: stageValue(node.stage),
      label: stringValue(node.label, "node.label"),
      state: stringValue(node.state, "node.state"),
      digest: stringValue(node.digest, "node.digest"),
      generation: numberValue(node.generation, "node.generation"),
      actorRole: stringValue(node.actorRole, "node.actorRole"),
      ...(node.costMinor === undefined
        ? {}
        : { costMinor: stringValue(node.costMinor, "node.costMinor") }),
      ...(node.currency === undefined
        ? {}
        : { currency: stringValue(node.currency, "node.currency") }),
    };
  });
  const edges = arrayValue(root.edges, "edges").map((entry) => {
    const edge = exactObject(entry, ["from", "to"]);
    return {
      from: stringValue(edge.from, "edge.from"),
      to: stringValue(edge.to, "edge.to"),
    };
  });
  const snapshot: PublicDeliveryLineageDto = {
    schemaVersion: numberValue(root.schemaVersion, "schemaVersion") as 1,
    serverRedacted: true,
    projectLabel: stringValue(root.projectLabel, "projectLabel"),
    revision: numberValue(root.revision, "revision"),
    nodes,
    edges,
    blockers: arrayValue(root.blockers, "blockers").map((entry) =>
      stringValue(entry, "blocker"),
    ),
    adapterReady: root.adapterReady,
    authorityGeneration: numberValue(root.authorityGeneration, "authorityGeneration"),
    readAtMs: numberValue(root.readAtMs, "readAtMs"),
  };
  const failures = validateLineage(snapshot);
  if (failures.length > 0) throw new Error(`invalid delivery lineage: ${failures.join("; ")}`);
  return snapshot;
}

export function shortDigest(value: string): string {
  return SHA256.test(value) ? `${value.slice(0, 10)}...${value.slice(-6)}` : "invalid";
}

export function formatMinorUnits(value: string, currency: string): string {
  if (!/^(?:0|[1-9][0-9]*)$/.test(value) || currency !== "USD") return "Cost invalid";
  const minor = BigInt(value);
  const major = minor / 100n;
  const cents = (minor % 100n).toString().padStart(2, "0");
  return `${currency} ${major}.${cents}`;
}

function exactObject(
  value: unknown,
  required: string[],
  optional: string[] = [],
): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("delivery lineage record is not an object");
  }
  const object = value as Record<string, unknown>;
  const allowed = new Set([...required, ...optional]);
  if (required.some((key) => !(key in object)) || Object.keys(object).some((key) => !allowed.has(key))) {
    throw new Error("delivery lineage record has missing or unknown fields");
  }
  return object;
}

function arrayValue(value: unknown, field: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`${field} is not an array`);
  return value;
}

function stringValue(value: unknown, field: string): string {
  if (typeof value !== "string") throw new Error(`${field} is not a string`);
  return value;
}

function numberValue(value: unknown, field: string): number {
  if (typeof value !== "number") throw new Error(`${field} is not a number`);
  return value;
}

function stageValue(value: unknown): DeliveryStage {
  const supported: DeliveryStage[] = [
    "customer_request", "agreement", "project", "work_item", "participant",
    "decision", "handoff", "blocker", "candidate", "qa", "workbench",
    "artifact", "review", "test", "finding", "approval", "manifest",
    "release", "delivery", "acceptance", "closeout", "rollback",
  ];
  if (typeof value !== "string" || !supported.includes(value as DeliveryStage)) {
    throw new Error("node.stage is unsupported");
  }
  return value as DeliveryStage;
}
