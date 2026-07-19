Conduct a compact setup interview and produce a deterministic `GaiaSpec`.

Collect only fields required to validate and generate a runnable Sentinel company:
company name, company type, city, address, agent count, seed, shift model, time scale, departments, culture axes, mission, and values.

Build `<GAIA_SPEC_JSON>` with this exact shape:

```json
{
  "company_name": "Example GmbH",
  "company_type": "software_agency",
  "city": "Vienna",
  "address": "Example Street 1",
  "agent_count": 4,
  "seed": 1,
  "shift_model": "hybrid",
  "time_scale": 1.0,
  "departments": [
    {"name": "Operations", "weight": 1, "roles": ["Operator"]}
  ],
  "culture": {
    "formality": 0.5,
    "collaboration": 0.5,
    "conflict_level": 0.5,
    "innovation": 0.5,
    "diversity": 0.5,
    "mission": "Example mission",
    "values": ["Example value"]
  }
}
```

Allowed `company_type` values are `software_agency`, `manufacturing`, `healthcare`, and `generic`. Allowed `shift_model` values are `office_hours`, `three_shift`, and `hybrid`. Keep `mission` and `values` inside `culture`, use `conflict_level` exactly, and do not add top-level fields outside this shape.

When information is missing, return a short `needs_input` response with the next questions. When complete, call the deterministic generator command supplied in the runtime instructions exactly once, then return a short `complete` response with its JSON result. Do not create files with shell redirection. The generated company must include `company-context.md`. Applying generated configuration to a running company requires a separate explicit operator request and the normal `sentinel-ctl --confirm` gate.
