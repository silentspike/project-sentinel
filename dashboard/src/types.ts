// DB-Row-Typen (exakt wie ReadModelStore Schema in sentinel-projection/src/store.rs)

export interface AgentRow {
  agent_id: number;
  name: string;
  role: string;
  shift_set: number;
  status: string;
  current_room: string | null;
  in_transit: number;
  transit_target: string | null;
  last_action: string | null;
  last_action_tick: number | null;
  hunger: number;
  energy: number;
  stress: number;
  bladder: number;
  social_need: number;
  caffeine_mg: number;
  mood: string | null;
  last_event_id: number;
  updated_at: number;
}

export interface RoomRow {
  room_id: string;
  occupant_count: number;
  transit_count: number;
  active_chaos: string | null;
  active_smells: string | null;
  temperature: number | null;
  co2_ppm: number | null;
  noise_db: number | null;
  last_event_tick: number | null;
  last_event_id: number;
  updated_at: number;
}

export interface KpiRow {
  bucket_start: number;
  active_agents: number;
  total_actions: number;
  total_transits: number;
  chaos_events: number;
  tick_count: number;
  shift_changes: number;
  nightrun_events: number;
  last_event_id: number;
  updated_at: number;
}

// Statische Room-Metadata (aus config/rooms.toml)

export interface RoomMeta {
  id: string;
  name: string;
  floor: number;
  capacity: number;
  room_type: string;
  adjacent: string[];
  department?: string;
  has_coffee_machine?: boolean;
}

// API-Response-Typen

export interface AgentListItem {
  id: number;
  name: string;
  role: string;
  status: string;
  current_room: string | null;
  room_name: string | null;
  in_transit: boolean;
  transit_target: string | null;
  last_action: string | null;
  last_action_tick: number | null;
  hunger: number;
  energy: number;
  stress: number;
  bladder: number;
  social_need: number;
  caffeine_mg: number;
  mood: string | null;
  stalled: boolean;
}

export interface AgentDetail extends AgentListItem {
  shift_set: number;
  last_action: string | null;
  last_action_tick: number | null;
  last_event_id: number;
}

export interface RoomResponse {
  id: string;
  name: string;
  floor: number;
  capacity: number;
  room_type: string;
  occupant_count: number;
  transit_count: number;
  active_chaos: unknown | null;
  active_smells: unknown | null;
  temperature: number | null;
  co2_ppm: number | null;
  noise_db: number | null;
  last_event_tick: number | null;
  occupants: string[];
}

export interface RoomPhysicsHistoryPoint {
  tick: number;
  timestamp_ms: number;
  temperature: number | null;
  co2_ppm: number | null;
  noise_db: number | null;
  occupant_count: number;
}

export interface RoomStimulusHistoryItem {
  event_id: string;
  room_id: string;
  stimulus_type: string;
  delta: number;
  description: string;
  tick: number;
  timestamp_ms: number;
}

export interface RoomReactionItem {
  event_id: string;
  agent_id: string;
  agent_name: string;
  action_type: string;
  content: string | null;
  target_room: string | null;
  tick: number;
  timestamp_ms: number;
  correlation_id: string;
  chaos_event_id: string | null;
  chaos_type: string | null;
  chaos_description: string | null;
  chaos_tick: number | null;
  stimulus_event_id: string | null;
  stimulus_type: string | null;
  stimulus_description: string | null;
  stimulus_tick: number | null;
}

export interface RoomDetailResponse extends RoomResponse {
  physics_history: RoomPhysicsHistoryPoint[];
  chaos_history: ChaosEventItem[];
  stimulus_history: RoomStimulusHistoryItem[];
  recent_reactions: RoomReactionItem[];
  reaction_window_ticks: number;
}

export interface MetricsResponse {
  active_agents: number;
  total_actions: number;
  total_transits: number;
  chaos_events: number;
  tick_count: number;
  shift_changes: number;
  nightrun_events: number;
  bucket_start: number | null;
  uptime: number;
  total_events: number;
  event_rate_per_min: number;
}

// ── Chaos Event Feed ────────────────────────────

export interface ChaosEventItem {
  id: number;
  event_id: string;
  chaos_type: string;
  room_id: string | null;
  description: string;
  tick: number;
  timestamp_ms: number;
}

// ── Chat Messages ───────────────────────────────

export interface ChatMessage {
  id: number;
  event_id: string;
  agent_id: string;
  agent_name: string;
  action_type: string;
  content: string | null;
  target_room: string | null;
  tick: number;
  timestamp_ms: number;
}

export interface HealthResponse {
  status: string;
  uptime: number;
  projection_lag: number;
}

export interface PlatformAnalysisItem {
  event_id: string;
  aggregate_id: string;
  trigger: string;
  severity: string;
  summary: string;
  recommendation: string;
  suggested_action: string | null;
  target: string;
  provider: string | null;
  model: string | null;
  unresolved_keys: string[];
  parameters: Record<string, unknown>;
  tick: number;
  timestamp_ms: number;
}

export interface PlatformStateAgent {
  agent_id: number;
  aggregate_id: string;
  name: string;
  last_activity_tick: number;
  cgroup_path: string;
  current_profile: string;
}

export interface PlatformStateResponse {
  current_tick: number;
  stall_recent_activity_grace_ticks: number;
  llm_enabled: boolean;
  llm_analysis_interval_secs: number;
  llm_retry_delay_secs: number;
  last_analysis_tick: number | null;
  last_analysis_trigger: string | null;
  last_scheduled_analysis_tick: number | null;
  unresolved_counts: Record<string, number>;
  threshold_overrides: Record<string, unknown>;
  resource_profiles: Record<string, string>;
  agents: PlatformStateAgent[];
}

// ── EventStore Row (raw) ──────────────────────────

export interface EventRow {
  id: number;
  event_id: string;
  event_type: string;
  aggregate_id: string;
  payload: string;
  correlation_id: string;
  causation_id: string | null;
  tick: number;
  timestamp_ms: number;
  compensation_type: string;
}

// ── Personality Evolution Row ─────────────────────

export interface EvolutionRow {
  id: number;
  agent_id: string;
  tick: number;
  field: string;
  change_type: string;
  old_value: string | null;
  new_value: string;
  reason: string;
  nmda_score: number | null;
  source: string;
  created_at_ms: number;
}

// ── Cockpit Types ─────────────────────────────────

export type IncidentSeverity = "critical" | "high" | "medium" | "low";
export type IncidentStatus = "active" | "resolved" | "pending" | "failed";

export interface CockpitAction {
  event_id: string;
  event_type: string;
  agent_id: string;
  summary: string;
  tick: number;
}

export interface CockpitIncident {
  id: string;
  source: "event" | "evolution";
  incident_type: string;
  severity: IncidentSeverity;
  status: IncidentStatus;
  agent_id: string | null;
  room_id: string | null;
  summary: string;
  tick: number;
  timestamp_ms: number;
  actions: CockpitAction[];
  outcome: string | null;
}

export interface SloViolation {
  name: string;
  current_value: number;
  threshold: number;
  severity: IncidentSeverity;
  description: string;
}

export interface CockpitResponse {
  incidents: CockpitIncident[];
  slo_violations: SloViolation[];
  total_active: number;
  total_resolved_24h: number;
}
