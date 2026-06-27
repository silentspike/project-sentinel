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

export async function apiText(path: string, init: RequestInit = {}): Promise<string> {
  const response = await fetch(path, {
    credentials: "include",
    ...init,
    headers: {
      ...init.headers,
    },
  });
  if (!response.ok) {
    const payload = await response.json().catch(() => null);
    const message =
      payload && typeof payload === "object" && "error" in payload
        ? String((payload as { error: unknown }).error)
        : response.statusText;
    throw new ApiError(response.status, message);
  }
  return response.text();
}

export function postJson<T>(path: string, body: unknown): Promise<T> {
  return apiJson<T>(path, { method: "POST", body: JSON.stringify(body) });
}

export function patchJson<T>(path: string, body: unknown): Promise<T> {
  return apiJson<T>(path, { method: "PATCH", body: JSON.stringify(body) });
}

export function deleteJson<T>(path: string, body: unknown): Promise<T> {
  return apiJson<T>(path, { method: "DELETE", body: JSON.stringify(body) });
}

export function putJson<T>(path: string, body: unknown): Promise<T> {
  return apiJson<T>(path, { method: "PUT", body: JSON.stringify(body) });
}

// #429: Synthesis Rules editor + Request Inspector types.
export interface SynthesisRule {
  name: string;
  enabled: boolean;
}

export interface TrafficResponse {
  request_id: string;
  request_class?: string;
  provider: string;
  model?: string;
  agent_id?: string;
  agent_name?: string;
  content: string;
  logged_at: string;
  decision?: string;
  rule?: string;
  fourth_wall?: string;
}

export interface JudgeAlert {
  // The events.db aggregate_id ("AGENT-NN") — the inspector's agent-level join key.
  agent_id: string;
  alert_type: string;
  severity: string;
  score: number;
  details: string;
  timestamp_ms: number;
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

export interface PhaseRow {
  phase: string;
  p50_ms: number;
  p95_ms: number;
  count: number;
  sum_ms: number;
  avg_ms: number;
}

export interface PhaseMetrics {
  available: boolean;
  phases: PhaseRow[];
  prometheus?: string;
}

// #427: cache-aware cost/token rows. `key` is the agent id ("AGENT-NN"), the tier
// name, or the minute-bucket start (from /api/cost, the CostHandler projection).
export interface CostRow {
  key: string;
  input_tokens: number;
  output_tokens: number;
  cache_read: number;
  cache_creation: number;
  cost_usd: number;
  call_count: number;
}

export interface CostStats {
  by_agent: CostRow[];
  by_tier: CostRow[];
  time_series: CostRow[];
  projection?: string;
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

export interface ControlConfig {
  primary_provider?: string;
  rate_limit_rps?: number;
  temperature?: number;
  max_tokens?: number;
  agent_overrides?: Record<string, string>;
  personality_guard_enabled?: boolean;
  drift_threshold?: number;
  quality_gate_enabled?: boolean;
  quality_threshold?: number;
  quality_max_regen?: number;
  narrative_nudge?: string;
  [key: string]: unknown;
}

export interface ControlStatus {
  connected: boolean;
  paused: boolean;
  config: ControlConfig | null;
  health: Record<string, unknown> | null;
  saved_rate_limit?: number | null;
  gateway?: "ok" | "offline" | string;
}

export interface TrafficStats {
  primary_provider?: string;
  internal_primary_provider?: string;
  external_mitm_provider?: string;
  current_cost_usd?: number;
  estimated_savings_usd?: number;
  projected_daily_cost_usd?: number;
  projected_daily_savings_usd?: number;
  avg_forward_cost_usd?: number;
  forward_calls?: number;
  synthesis_count?: number;
  synthesis_rate?: number;
  tick_sync_enabled?: boolean;
  tick_sync_runtime_enabled?: boolean;
  tick_sync_pending?: number;
  synthesis_enabled?: boolean;
  sequencing_enabled?: boolean;
  apicp_enabled?: boolean;
  active_patterns?: number;
  queue_depth?: number | null;
  active_forward_calls?: number | null;
  pending_intercepts?: number;
  pending_response_intercepts?: number;
  response_log_entries?: number;
  tick_sync_timeout_ms?: number | null;
  p3_timeout_ms?: number | null;
  intercept_mode?: string;
  [key: string]: unknown;
}

export interface PlatformAnalysis {
  event_id?: string;
  trigger?: string;
  severity?: string;
  summary?: string;
  recommendation?: string;
  suggested_action?: string;
  target?: string;
  aggregate_id?: string;
  provider?: string | null;
  model?: string | null;
  tick?: number;
  unresolved_keys?: string[];
  [key: string]: unknown;
}

export interface PlatformAgentState {
  agent_id?: number | string;
  name?: string;
  aggregate_id?: string;
  current_profile?: string;
  last_activity_tick?: number | null;
  cgroup_path?: string | null;
  [key: string]: unknown;
}

export interface PlatformState {
  current_tick?: number;
  llm_enabled?: boolean;
  llm_analysis_interval_secs?: number;
  llm_retry_delay_secs?: number;
  last_analysis_tick?: number | null;
  last_analysis_trigger?: string | null;
  last_scheduled_analysis_tick?: number | null;
  stall_recent_activity_grace_ticks?: number;
  unresolved_counts?: Record<string, unknown>;
  threshold_overrides?: Record<string, unknown>;
  agents?: PlatformAgentState[];
  [key: string]: unknown;
}

// #442 Gaia Console Memory Loop and explicit Claude session surfaces.
export interface GaiaAlert {
  alert_id: string;
  source_event_id: string;
  tick: number;
  timestamp_ms: number;
  trigger: string;
  severity: string;
  target: string;
  summary: string;
  recommendation: string;
  unresolved_keys: string[];
}

export interface GaiaAlertsResponse {
  alerts: GaiaAlert[];
  count: number;
  source: string;
}

export type GaiaSessionKind = "deep" | "setup_interview" | string;
export type GaiaSessionStatus = "started" | "succeeded" | "failed" | "timed_out" | string;

export interface ClaudeUsageSummary {
  input_tokens: number;
  output_tokens: number;
  cache_read_input_tokens: number;
  cache_creation_input_tokens: number;
  total_cost_usd?: number | null;
}

export interface GaiaSessionIndexEntry {
  gaia_session_id: string;
  claude_session_id?: string | null;
  kind: GaiaSessionKind;
  status: GaiaSessionStatus;
  stream_path: string;
  started_at_ms: number;
  finished_at_ms?: number | null;
  exit_code?: number | null;
  usage: ClaudeUsageSummary;
}

export interface GaiaSessionsResponse {
  sessions: GaiaSessionIndexEntry[];
  count: number;
  source: string;
}

export interface GaiaSessionRun {
  entry: GaiaSessionIndexEntry;
  session_dir: string;
  prompt_path: string;
  stderr_path: string;
}

export interface SnapshotInfo {
  id: string;
  tier: string;
  created_at_ms: number;
  tick: number;
  sim_hour?: number;
  payload_size_bytes?: number | null;
  [key: string]: unknown;
}

export interface SnapshotRoomState {
  room_id?: string;
  name: string;
  occupant_count: number;
}

export interface SnapshotWorldState {
  snapshot_id: string;
  tier: string;
  created_at_ms: number;
  tick: number;
  sim_hour?: number;
  last_event_id?: number;
  active_agent_count?: number | null;
  present_agent_count: number;
  room_count: number;
  rooms: SnapshotRoomState[];
}

// ── Config editor schemas (#421/#422/#423) ──
// These mirror the Rust serde JSON exactly. SSOT of the SHAPE = the Rust structs:
//   GaiaSpec (services/sentinel-gaia/src/lib.rs), AgentConfig (crates/sentinel-common/src/agent_config.rs),
//   BuildingConfig (crates/sentinel-common/src/room.rs). Enums serialize snake_case.

export type CompanyType = "software_agency" | "manufacturing" | "healthcare" | "generic";
export type ShiftModel = "office_hours" | "three_shift" | "hybrid";

export interface CultureSpec {
  formality: number;
  collaboration: number;
  conflict_level: number;
  innovation: number;
  diversity: number;
  mission: string;
  values: string[];
}

export interface DepartmentSpec {
  name: string;
  weight: number;
  roles: string[];
}

export interface GaiaSpec {
  company_name: string;
  company_type: CompanyType;
  city: string;
  address: string;
  agent_count: number;
  seed: number;
  shift_model: ShiftModel;
  time_scale: number;
  departments: DepartmentSpec[];
  culture: CultureSpec;
}

export interface IdentityConfig {
  id: number;
  name: string;
  role: string;
  department: string;
  shift_set: number;
  kpis: string[];
  reports_to?: string | null;
  direct_reports: string[];
}

export interface PersonalityConfig {
  openness: number;
  conscientiousness: number;
  extraversion: number;
  agreeableness: number;
  neuroticism: number;
  caffeine_tolerance: number;
  morning_person: boolean;
}

export interface PreferencesConfig {
  favorite_room: string;
  coffee_preference: string;
  lunch_time: string;
}

export interface BackgroundConfig {
  bio: string;
  quirks: string[];
}

export interface RuntimeSelectionConfig {
  nano_runtime?: string | null;
}

export interface CapabilitiesConfig {
  tools: string[];
  sandbox_allowed_paths: string[];
}

export interface AgentConfig {
  identity: IdentityConfig;
  personality: PersonalityConfig;
  preferences: PreferencesConfig;
  background: BackgroundConfig;
  runtime: RuntimeSelectionConfig;
  capabilities: CapabilitiesConfig;
}

export type RoomType = "office" | "meeting" | "common" | "break" | "transit" | "bathroom";

export interface RoomConfig {
  id: string;
  name: string;
  floor: number;
  capacity: number;
  room_type: RoomType;
  adjacent: string[];
  department?: string | null;
  has_coffee_machine: boolean;
  has_printer: boolean;
}

export interface BuildingMeta {
  name: string;
  address: string;
  floors: number;
}

export interface BuildingConfig {
  building: BuildingMeta;
  rooms: RoomConfig[];
}

export interface GeneratePreview {
  summary: { agent_count: number; room_count: number; shift_distribution: Record<string, number> };
  agents: AgentConfig[];
  building: BuildingConfig;
}

export interface DaemonParams {
  content: string;
  max_agents: number | null;
  time_scale: number | null;
  tick_rate_ms: number | null;
}

// #428 Agent Deep View — read-only FS browser (sentinel-fs) + per-agent lifecycle.

/** One entry in a read-only agent FS directory listing. */
export interface FsEntry {
  name: string;
  inode: number;
  kind: "file" | "dir" | "symlink" | string;
  size: number;
  mode: number;
  mtime: number;
  /** Hex content hash (empty for directories/symlinks). */
  hash: string;
  /** How many inodes share the same CAS blob (dedup sharing; 0 for non-files). */
  refcount: number;
}

/** GET /api/control/agent/{id}/fs?inode=N — directory listing of an agent layer. */
export interface FsListing {
  accepted: boolean;
  agent_id: number;
  aggregate_id: string;
  inode: number;
  entries: FsEntry[];
  dedup_ratio_percent: number;
  cas_blob_count: number;
  dedup_savings_bytes: number;
}

/** GET /api/control/agent/{id}/fs/read?inode=N — file content (size-capped). */
export interface FsFileRead {
  accepted: boolean;
  agent_id: number;
  aggregate_id: string;
  inode: number;
  size: number;
  returned_bytes: number;
  truncated: boolean;
  hash: string;
  refcount: number;
  encoding: "utf8" | "hex" | string;
  content: string;
}

/** POST /api/control/agent/{id}/{stop,start,remove} — lifecycle result. */
export interface AgentLifecycleResult {
  accepted: boolean;
  agent_id: number;
  aggregate_id: string;
  action: "pause" | "resume" | "despawn" | string;
  new_status: string;
  affected_pids: number;
  outcome: "ok" | "invalid_transition" | "not_found" | string;
  note: string;
}

/** One row of the event log (GET /api/events?agent=AGENT-NN), used for activity charts. */
export interface EventRow {
  id: number;
  event_id: string;
  event_type: string;
  aggregate_id: string;
  payload: string;
  correlation_id?: string | null;
  causation_id?: string | null;
  tick: number;
  timestamp_ms: number;
  compensation_type?: string | null;
}

/** GET /api/events?agent=AGENT-NN — per-agent event window for sparkline + tool donut. */
export interface EventsResponse {
  events: EventRow[];
  total: number;
  limit: number;
  offset: number;
  events_db: string;
}
