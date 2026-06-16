import { describe, it, expect } from "vitest";
import type { AgentConfig, BuildingConfig, GaiaSpec } from "../src/api";
import {
  validateGaiaSpec,
  validateGaiaStep,
  validatePersonality,
  validateAgentRequired,
  validateAdjacency,
} from "../src/views/config/validation";

const culture = () => ({
  formality: 0.5,
  collaboration: 0.5,
  conflict_level: 0.5,
  innovation: 0.5,
  diversity: 0.5,
  mission: "",
  values: [],
});

const validSpec = (): GaiaSpec => ({
  company_name: "TestCo",
  company_type: "software_agency",
  city: "Nuernberg",
  address: "Strasse 1",
  agent_count: 10,
  seed: 42,
  shift_model: "hybrid",
  time_scale: 1.0,
  departments: [],
  culture: culture(),
});

const validAgent = (): AgentConfig => ({
  identity: {
    id: 1,
    name: "Thomas",
    role: "CEO",
    department: "Leitung",
    shift_set: 1,
    kpis: [],
    reports_to: null,
    direct_reports: [],
  },
  personality: {
    openness: 0.5,
    conscientiousness: 0.5,
    extraversion: 0.5,
    agreeableness: 0.5,
    neuroticism: 0.5,
    caffeine_tolerance: 0.5,
    morning_person: true,
  },
  preferences: { favorite_room: "buero", coffee_preference: "espresso", lunch_time: "12:00" },
  background: { bio: "Founder.", quirks: ["paces"] },
  runtime: { nano_runtime: null },
  capabilities: { tools: [], sandbox_allowed_paths: [] },
});

const room = (id: string, adjacent: string[]): BuildingConfig["rooms"][number] => ({
  id,
  name: id,
  floor: 0,
  capacity: 10,
  room_type: "office",
  adjacent,
  department: null,
  has_coffee_machine: false,
  has_printer: false,
});

const building = (rooms: BuildingConfig["rooms"]): BuildingConfig => ({
  building: { name: "B", address: "A", floors: 1 },
  rooms,
});

describe("validateGaiaSpec (mirrors GaiaSpec::validate)", () => {
  it("accepts a valid spec", () => {
    expect(validateGaiaSpec(validSpec())).toEqual([]);
  });
  it("rejects empty company_name / agent_count<1 / time_scale<=0", () => {
    expect(validateGaiaSpec({ ...validSpec(), company_name: "  " })).toContain(
      "company_name must not be empty",
    );
    expect(validateGaiaSpec({ ...validSpec(), agent_count: 0 })).toContain(
      "agent_count must be at least 1",
    );
    expect(validateGaiaSpec({ ...validSpec(), time_scale: 0 })).toContain("time_scale must be > 0.0");
  });
  it("rejects out-of-range culture axes and zero-weight departments", () => {
    expect(
      validateGaiaSpec({ ...validSpec(), culture: { ...culture(), innovation: 1.5 } }),
    ).toContain("culture.innovation must be in [0.0, 1.0]");
    expect(
      validateGaiaSpec({ ...validSpec(), departments: [{ name: "Dev", weight: 0, roles: [] }] }),
    ).toContain("department 'Dev' weight must be > 0");
  });
});

describe("validateGaiaStep", () => {
  it("step 1 needs a name + agent_count>=1", () => {
    expect(validateGaiaStep(1, validSpec())).toBe(true);
    expect(validateGaiaStep(1, { ...validSpec(), company_name: "" })).toBe(false);
    expect(validateGaiaStep(1, { ...validSpec(), agent_count: 0 })).toBe(false);
  });
  it("step 4 needs culture axes in unit range", () => {
    expect(validateGaiaStep(4, validSpec())).toBe(true);
    expect(validateGaiaStep(4, { ...validSpec(), culture: { ...culture(), diversity: 2 } })).toBe(
      false,
    );
  });
});

describe("validatePersonality (mirrors PersonalityConfig::validate)", () => {
  it("accepts in-range Big Five", () => {
    expect(validatePersonality(validAgent().personality)).toEqual([]);
  });
  it("rejects openness > 1.0", () => {
    expect(validatePersonality({ ...validAgent().personality, openness: 2.0 })).toContain(
      "openness value 2 out of range [0.0, 1.0]",
    );
  });
});

describe("validateAgentRequired", () => {
  it("accepts a complete agent", () => {
    expect(validateAgentRequired(validAgent())).toEqual([]);
  });
  it("rejects empty name and missing quirks", () => {
    const a = validAgent();
    a.identity.name = " ";
    a.background.quirks = [];
    const errs = validateAgentRequired(a);
    expect(errs).toContain("name must not be empty");
    expect(errs).toContain("at least one quirk required");
  });
});

describe("validateAdjacency (mirrors BuildingConfig::validate)", () => {
  it("accepts bidirectional adjacency", () => {
    expect(validateAdjacency(building([room("a", ["b"]), room("b", ["a"])]))).toEqual([]);
  });
  it("rejects one-sided adjacency", () => {
    const errs = validateAdjacency(building([room("a", ["b"]), room("b", [])]));
    expect(errs.some((e) => e.includes("not bidirectional"))).toBe(true);
  });
  it("rejects dangling reference and duplicate ids", () => {
    expect(
      validateAdjacency(building([room("a", ["ghost"]), room("b", [])])).some((e) =>
        e.includes("non-existent"),
      ),
    ).toBe(true);
    expect(
      validateAdjacency(building([room("a", []), room("a", [])])).some((e) =>
        e.includes("Duplicate room ID"),
      ),
    ).toBe(true);
  });
});
