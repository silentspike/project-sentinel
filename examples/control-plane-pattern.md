# Control Plane Pattern — three independent OODA loops, one runtime

> Sentinel runs three independent observe / decide / act / verify loops
> side-by-side. Each owns one decision domain; none reach across. This
> walkthrough exercises that the boundaries hold: an Agent-CP decision
> never touches a Platform-CP concern, and vice versa.

## Use case

You are evaluating runtime governance. The claim is that
*Agent CP / Platform CP / API CP* are not three labels on one process —
they are three loops with their own state, their own decision domain,
and their own audit trail. This walkthrough demonstrates that
empirically.

## Pre-conditions

- Demo stack is up (`make demo` running)
- `curl` and `jq` available locally

## Commands

```bash
# 1. Pull the Agent-CP decision log (bio + perception decisions).
#    Typical: shift-based scheduling, perception injection.
curl -fsS http://127.0.0.1:18084/v1/control-plane/agent/decisions?limit=10 \
  | jq '.[] | {ts, agent_id, decision_type, target}'

# 2. Pull the Platform-CP decision log (infra health, restarts, scaling).
#    Typical: agent-stall restarts, projection-rebuild triggers.
curl -fsS http://127.0.0.1:18084/v1/control-plane/platform/decisions?limit=10 \
  | jq '.[] | {ts, decision_type, component, action}'

# 3. Pull the API-CP decision log (cost routing, provider switches).
#    Typical: provider failover, rate-limit decisions.
curl -fsS http://127.0.0.1:18080/internal/control-plane/api/decisions?limit=10 \
  | jq '.[] | {ts, decision_type, provider, reason}'

# 4. Verify isolation: no Agent-CP decision should reference a platform-
#    component, no Platform-CP should reference an agent_id, no API-CP
#    should reference either. Cross-pollination = boundary breach.
echo "Cross-pollination check (expect empty):"
curl -fsS http://127.0.0.1:18084/v1/control-plane/agent/decisions?limit=100 \
  | jq '.[] | select(.target | test("daemon|gateway|projection|nightrun"))'
```

## Expected output

Each control plane returns its own decision-history. The cross-pollination
check returns no matches — Agent-CP decisions never target a platform
component name.

## What this demonstrates

- **Decision-domain ownership.** Each control plane has its own decision
  ledger. No shared mutable state, no cross-domain writes.
- **Observable.** Every decision is logged with `ts`, `decision_type`,
  and the target entity. Audit-friendly out of the box.
- **TOGAF cluster 05b alignment.** Three loops correspond to TOGAF
  v22.1 control-plane decomposition.

## When this fails

- A decision shows up in two ledgers at the same `ts` → indicates a
  shared write path (boundary breach). File a bug.
- An Agent-CP decision targets `daemon` or `gateway` → control-plane
  fan-out, not isolated. File a bug.

## See also

- `docs/governance.md` — full mapping of governance mechanisms to code paths
- `crates/sentinel-runtime/` — Agent-CP loop
- `services/sentinel-daemon/src/platform_cp.rs` — Platform-CP loop
- `cmd/cortex-gateway/internal/apicp/` — API-CP loop
- [docs/workshop-agent-runtime-governance.md](../docs/workshop-agent-runtime-governance.md) Section 3 (Hands-on)
