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
  last_event_id: number;
  updated_at: number;
}

export interface RoomRow {
  room_id: string;
  occupant_count: number;
  transit_count: number;
  active_chaos: string | null;
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
  last_event_tick: number | null;
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
}

export interface HealthResponse {
  status: string;
  uptime: number;
  projection_lag: number;
}
