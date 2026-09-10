export interface CustomerIdentity {
  schema_version: number;
  principal_id: string;
  tenant_id: string;
  customer_id: string;
}

export interface CustomerRequest {
  request_id: string;
  summary_ref: string;
  desired_outcome: string;
  constraints: string[];
  state: string;
  version: number;
  proposal_ids: string[];
  clarifications: { question_ref: string; answer_ref: string }[];
  feedback: { feedback_ref: string }[];
}

export interface Proposal {
  proposal_id: string;
  request_id: string;
  proposal_digest: string;
  scope: string;
  deliverables: string[];
  exclusions: string[];
  acceptance_criteria: string[];
  assumptions: string[];
  cost_ceiling_micros: number;
  expires_at_unix_ms: number;
}

export interface DeliveryReference { id: string; generation: number; digest: string }
export interface CustomerDelivery {
  delivery: DeliveryReference;
  release: DeliveryReference;
  state: string;
  release_state: string;
  issued_at_ms: number;
  expires_at_ms: number;
  preview_digest: string;
}
export interface ProjectProgress { project_id: string; request_id: string; state: string; version: number; work_items: { work_item_id: string; state: string }[]; deliveries?: CustomerDelivery[] }
export interface Overview { requests: CustomerRequest[]; proposals: Proposal[]; projects?: ProjectProgress[] }
export interface PendingCommand { operation_id: string; command: Record<string, unknown>; dispatched?: boolean }

export class CustomerApiError extends Error {
  constructor(public status: number, public code: string) { super(code); }
}

export async function customerFetch<T>(path: string, body?: unknown): Promise<T> {
  const response = await fetch(`/api/customer/${path}`, {
    method: body === undefined ? "GET" : "POST",
    credentials: "same-origin",
    cache: "no-store",
    headers: body === undefined ? undefined : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(15_000),
  });
  const value = await response.json();
  if (!response.ok) throw new CustomerApiError(response.status, typeof value.error === "string" ? value.error : "customer_request_failed");
  return value as T;
}

export function pendingKey(identity: CustomerIdentity): string {
  return `sentinel.customer.pending.v1:${JSON.stringify([identity.tenant_id, identity.customer_id, identity.principal_id])}`;
}

export function readPending(storage: Storage, identity: CustomerIdentity): PendingCommand | null {
  const raw = storage.getItem(pendingKey(identity));
  if (!raw) return null;
  const value = JSON.parse(raw) as PendingCommand;
  if (!value || typeof value.operation_id !== "string" || !/^[0-9a-f-]{36}$/.test(value.operation_id)
    || !value.command || typeof value.command !== "object" || typeof value.command.command !== "string") {
    throw new Error("customer_pending_record_invalid");
  }
  return value;
}

export function reserveCommand(storage: Storage, identity: CustomerIdentity, command: Record<string, unknown>): PendingCommand {
  if (readPending(storage, identity)) throw new Error("customer_command_pending");
  const value = { operation_id: crypto.randomUUID(), command, dispatched: false };
  // Persist before dispatch. A failed write must never send a customer mutation.
  storage.setItem(pendingKey(identity), JSON.stringify(value));
  return value;
}

export async function dispatchReserved(storage: Storage, identity: CustomerIdentity, pending: PendingCommand, send: (body: Pick<PendingCommand, "operation_id" | "command">) => Promise<unknown>): Promise<void> {
  const current = readPending(storage, identity);
  if (!current || current.operation_id !== pending.operation_id || JSON.stringify(current.command) !== JSON.stringify(pending.command)) throw new Error("customer_pending_record_changed");
  const previouslyDispatched = current.dispatched !== false;
  storage.setItem(pendingKey(identity), JSON.stringify({ ...current, dispatched: true }));
  try {
    await send({ operation_id: current.operation_id, command: current.command });
    storage.removeItem(pendingKey(identity));
  } catch (cause) {
    // Only the first, conclusively rejected attempt can be discarded. A later
    // authorization/version error does not resolve an earlier unknown outcome.
    if (!previouslyDispatched && cause instanceof CustomerApiError && [400, 401, 403, 404, 409, 422].includes(cause.status)) storage.removeItem(pendingKey(identity));
    throw cause;
  }
}
