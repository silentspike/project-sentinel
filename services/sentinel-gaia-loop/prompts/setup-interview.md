Conduct a compact setup interview and produce a deterministic `GaiaSpec`.

Collect only fields required to validate and generate a runnable Sentinel company:
company name, company type, city, address, agent count, seed, shift model, time scale, departments, culture axes, mission, and values.

When information is missing, return a short `needs_input` response with the next questions. When complete, return a `complete` response with the structured `GaiaSpec`. The generated company must include `company-context.md`.
