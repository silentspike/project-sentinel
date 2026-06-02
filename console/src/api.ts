export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

export async function apiJson<T>(path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(path, {
    credentials: "include",
    ...init,
    headers: {
      ...(init.body ? { "content-type": "application/json" } : {}),
      ...init.headers,
    },
  });
  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    const message =
      payload && typeof payload === "object" && "error" in payload
        ? String((payload as { error: unknown }).error)
        : response.statusText;
    throw new ApiError(response.status, message);
  }
  return payload as T;
}

export function postJson<T>(path: string, body: unknown): Promise<T> {
  return apiJson<T>(path, { method: "POST", body: JSON.stringify(body) });
}

export interface EbpfMetrics {
  available: boolean;
  mode: string;
  stalled_count: number;
  stalled_agents: { agent: string; seconds?: number }[];
  collection_cycle_us?: number;
  ring_buffer_drops?: number;
  io_read_bytes?: number;
  io_write_bytes?: number;
  avg_stress?: number;
  prometheus?: string;
}

export interface PipelineProvider {
  provider: string;
  latency_avg_s: number;
  latency_count: number;
  requests_ok: number;
  requests_error: number;
  tokens_input: number;
  tokens_output: number;
}

export interface PipelineMetrics {
  available: boolean;
  gateway?: string;
  providers: PipelineProvider[];
}

export interface TickMetrics {
  available: boolean;
  tick_duration_ms: number;
  tick_rate_effective_ms: number;
  psi_cpu_avg10: number;
  psi_mem_avg10: number;
  psi_io_avg10: number;
  prometheus?: string;
}

export type IncidentSeverity = "critical" | "high" | "medium" | "low" | string;
export type IncidentStatus = "active" | "pending" | "resolved" | "failed" | string;

export interface CockpitIncident {
  id: string;
  source?: string;
  incident_type: string;
  severity: IncidentSeverity;
  status: IncidentStatus;
  agent_id?: string | number | null;
  room_id?: string | null;
  summary: string;
  tick: number;
  timestamp_ms: number;
  actions: { event_id: string; event_type: string; summary: string; tick: number }[];
  outcome: string | null;
}

export interface SloViolation {
  name: string;
  current_value: number;
  threshold: number;
}

export interface CockpitResponse {
  incidents: CockpitIncident[];
  slo_violations: SloViolation[];
  total_active: number;
  total_resolved_24h: number;
  events_db: "ok" | "offline" | string;
}
