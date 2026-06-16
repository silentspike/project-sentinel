// Client-side config validation — UX ONLY. The server (sentinel-dashboard-backend `validate_apply` →
// daemon `validate_config_apply`) is AUTHORITATIVE; this mirror only disables buttons / shows inline
// errors before a round-trip, so an invalid edit never reaches the daemon.
//
// ⚠️ DRIFT WARNING — the SSOT of the RULES is the Rust validators:
//   GaiaSpec::validate          (services/sentinel-gaia/src/lib.rs)
//   PersonalityConfig::validate (crates/sentinel-common/src/agent_config.rs)
//   BuildingConfig::validate    (crates/sentinel-common/src/room.rs)
// If the Rust rules change, update this mirror. It is never a security / 1:n boundary (the server gates).

import type { AgentConfig, BuildingConfig, GaiaSpec, PersonalityConfig } from "../../api";

const inUnit = (v: number): boolean => Number.isFinite(v) && v >= 0 && v <= 1;

/** Mirrors `GaiaSpec::validate` — returns all errors (empty = valid). */
export function validateGaiaSpec(spec: GaiaSpec): string[] {
  const errors: string[] = [];
  if (!spec.company_name.trim()) errors.push("company_name must not be empty");
  if (!Number.isFinite(spec.agent_count) || spec.agent_count < 1)
    errors.push("agent_count must be at least 1");
  if (!Number.isFinite(spec.time_scale) || spec.time_scale <= 0)
    errors.push("time_scale must be > 0.0");
  for (const d of spec.departments) {
    if (!d.name.trim()) errors.push("department name must not be empty");
    if (!Number.isFinite(d.weight) || d.weight < 1)
      errors.push(`department '${d.name}' weight must be > 0`);
  }
  for (const [name, value] of [
    ["formality", spec.culture.formality],
    ["collaboration", spec.culture.collaboration],
    ["conflict_level", spec.culture.conflict_level],
    ["innovation", spec.culture.innovation],
    ["diversity", spec.culture.diversity],
  ] as const) {
    if (!inUnit(value)) errors.push(`culture.${name} must be in [0.0, 1.0]`);
  }
  return errors;
}

/** Per-step validity for the wizard "Next" button (1-based step index matching GaiaWizardView). */
export function validateGaiaStep(step: number, spec: GaiaSpec): boolean {
  switch (step) {
    case 1: // Company
      return spec.company_name.trim().length > 0 && spec.agent_count >= 1;
    case 2: // Shift / time
      return spec.time_scale > 0;
    case 3: // Departments (optional, but each present one must be valid)
      return spec.departments.every((d) => d.name.trim().length > 0 && d.weight >= 1);
    case 4: // Culture
      return [
        spec.culture.formality,
        spec.culture.collaboration,
        spec.culture.conflict_level,
        spec.culture.innovation,
        spec.culture.diversity,
      ].every(inUnit);
    default: // Preview / Success — nothing to validate
      return true;
  }
}

/** Mirrors `PersonalityConfig::validate` — all f32 in [0, 1]. */
export function validatePersonality(p: PersonalityConfig): string[] {
  const errors: string[] = [];
  for (const [name, value] of [
    ["openness", p.openness],
    ["conscientiousness", p.conscientiousness],
    ["extraversion", p.extraversion],
    ["agreeableness", p.agreeableness],
    ["neuroticism", p.neuroticism],
    ["caffeine_tolerance", p.caffeine_tolerance],
  ] as const) {
    if (!inUnit(value)) errors.push(`${name} value ${value} out of range [0.0, 1.0]`);
  }
  return errors;
}

/** Required non-empty fields (mirrors the `required_fields_not_empty` invariant). */
export function validateAgentRequired(agent: AgentConfig): string[] {
  const errors: string[] = [];
  if (!agent.identity.name.trim()) errors.push("name must not be empty");
  if (!agent.identity.role.trim()) errors.push("role must not be empty");
  if (!agent.identity.department.trim()) errors.push("department must not be empty");
  if (!agent.preferences.favorite_room.trim()) errors.push("favorite_room must not be empty");
  if (!agent.background.bio.trim()) errors.push("bio must not be empty");
  if (agent.background.quirks.length === 0) errors.push("at least one quirk required");
  return errors;
}

/** Mirrors the `BuildingConfig::validate` adjacency rules: no dup ids, refs exist, bidirectional. */
export function validateAdjacency(building: BuildingConfig): string[] {
  const errors: string[] = [];
  const ids = new Set(building.rooms.map((r) => r.id));
  const seen = new Set<string>();
  for (const r of building.rooms) {
    if (seen.has(r.id)) errors.push(`Duplicate room ID: ${r.id}`);
    seen.add(r.id);
  }
  const adj = new Map(building.rooms.map((r) => [r.id, new Set(r.adjacent)]));
  for (const r of building.rooms) {
    for (const a of r.adjacent) {
      if (!ids.has(a)) {
        errors.push(`Room '${r.id}' references non-existent adjacent room '${a}'`);
        continue;
      }
      if (!adj.get(a)?.has(r.id)) {
        errors.push(
          `Adjacency not bidirectional: '${r.id}' → '${a}' exists, but '${a}' → '${r.id}' missing`,
        );
      }
    }
  }
  return errors;
}
