# Audit Replay Pattern — deterministic event-sourcing walkthrough

> Sentinel's event store is append-only. Any state at any point in time is
> the fold of events up to that point. This walkthrough shows how to save
> a snapshot, restart the daemon, and verify the replay produced the same
> state — the core deterministic-replay claim.

## Use case

You want to convince yourself (or a customer) that the runtime is
honestly auditable: stop the world mid-shift, restart, and confirm the
agents land back in the same rooms with the same bio-state. This is the
demo behind the *"event-sourced audit trails"* line in the README.

## Pre-conditions

- Demo stack is up (`make demo` running, dashboard reachable at
  `http://localhost:18000`)
- `curl` and `jq` available locally
- Operator API on `http://localhost:18084` reachable

## Commands

```bash
# 1. Take a snapshot mid-shift, label it "workshop-snap-1"
curl -fsS -X POST http://127.0.0.1:18084/v1/snapshot \
  -H 'content-type: application/json' \
  -d '{"label":"workshop-snap-1"}'

# 2. Read /api/agents BEFORE the restart — record names + rooms
curl -fsS http://127.0.0.1:18000/api/agents | jq '.[] | {name, room}' > /tmp/agents-before.json

# 3. Stop the daemon container
docker compose -f docker-compose.demo.yml stop daemon

# 4. Start it again — projection seeds itself from the snapshot
docker compose -f docker-compose.demo.yml start daemon
sleep 10

# 5. Read /api/agents AFTER the restart
curl -fsS http://127.0.0.1:18000/api/agents | jq '.[] | {name, room}' > /tmp/agents-after.json

# 6. Compare — should be identical
diff /tmp/agents-before.json /tmp/agents-after.json && echo "REPLAY IDENTICAL"
```

## Expected output

```
REPLAY IDENTICAL
```

## What this demonstrates

- **Append-only event store.** No state mutation between events.
- **Snapshot version monotonicity.** Each snapshot row in the Limbo
  `snapshots` table has a strictly increasing `version`.
- **Hash-chain witness.** `sentinel-nightrun` walks the event chain for a
  given `correlation_id` and computes a SHA-256 chain hash. Replaying the
  same events produces the same hash. Divergence = bug, not tolerance.

## When this fails

- Snapshot was taken mid-tick and the system was not quiescent —
  rare in practice. File a bug; the hash chain is the witness.
- Projection seeded from events instead of snapshot → diverges. Make
  sure the daemon has the snapshot file mounted into the container.

## See also

- `services/sentinel-nightrun/` — hash-chain implementation
- `crates/sentinel-limbo/src/snapshot.rs` — snapshot schema + monotonicity
- [docs/workshop-agent-runtime-governance.md](../docs/workshop-agent-runtime-governance.md) Section 3 (Hands-on)
