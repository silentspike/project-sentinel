# Hierarchy Tier Usage v2 Cutover

This runbook defines the fail-closed deployment order for issue #395. It does
not authorize a deployment, a service restart, a provider call, or a runtime
target. Gate C and a current Gate B reservation are prerequisites.

## Preconditions

- Gate C pins `config/cortex-gateway.toml` by Git blob OID, file SHA-256, and
  `cortex-catalog-v1` semantic digest. The normalized digest input includes
  provider ID/type, default, and the complete allowlist. The hierarchy-model
  mappings are validated fail-closed and remain pinned by the blob/file hashes,
  but are deliberately outside the semantic digest.
- Gate B names one isolated target, owner, exclusive time window, mutation
  scope, rollback owner, and the exactly-one-daemon invariant.
- The production reference and any target reserved by another issue remain out
  of scope.
- `SENTINEL_LLM_USAGE_V2_ENABLED` is absent or `false` on the daemon.
- No paid-provider request is made while the approved budget is USD 0.
- Baseline evidence records service state, restart counts, binary and config
  hashes, event-store boundary, projection offsets, and aggregate totals.

## Boundary queries

Run these queries against read-only snapshots or through an approved read-only
database session. The first two queries target the event-store database; the
remaining queries target the projection database. Record every result with the
target, timestamp, database path, and deployed revision.

```sql
SELECT COALESCE(MAX(id), 0) AS event_boundary FROM events;

SELECT projection_name, last_event_id
FROM projection_offsets
WHERE projection_name IN (
  'sentinel-projection',
  'sentinel-projection-cost-hierarchy-v2'
)
ORDER BY projection_name;
```

Projection database:

```sql
SELECT first_v2_event_id,
       last_usage_event_id,
       last_hierarchy_event_id,
       unattributed_v1_usage_events
FROM cost_hierarchy_projection_meta
WHERE id = 1;

SELECT hierarchy_tier,
       input_tokens,
       output_tokens,
       cache_read,
       cache_creation,
       cost_usd,
       call_count,
       last_event_id
FROM cost_by_hierarchy_tier
ORDER BY hierarchy_tier;
```

Confirm the actual event and offset table names from the deployed schema before
using the queries. A schema mismatch is a stop condition, not permission to
improvise a migration.

## Cutover order

1. Record boundary A from the event store while the usage-v2 producer is
   disabled.
2. Deploy the projection binary and additive read-model schema only. Do not
   enable authenticated caller traffic or the v2 producer in this step.
3. Run the established full projection rebuild mechanism. Wait until the
   independent `sentinel-projection-cost-hierarchy-v2` offset reaches boundary
   A. The unrelated global projection offset may already be ahead.
4. Verify at boundary A:
   - v1 usage events increased only `unattributed_v1_usage_events`;
   - `cost_by_hierarchy_tier` contains no inferred legacy rows;
   - the hierarchy offset, coverage metadata, and event boundary reconcile;
   - restarting or replaying the projection does not increment aggregates.
5. Deploy the gateway and all four authenticated callers with distinct
   owner-only credentials. Keep the daemon producer flag disabled. Use only
   mock or local-loop traffic.
6. If Ollama is the active provider, record the exact `name`, `model`, and
   non-empty content `digest` values returned by its token-free model inventory.
   The model IDs must exactly equal the immutable catalog allowlist, and gateway
   `/ready` must report `model_inventory_status=validated`. Missing, additional,
   duplicate, digest-less, or unreachable inventory fails readiness. Do not pull
   or replace a model unless the current Gate B mutation scope explicitly allows
   it.
7. Verify that `/internal/agent-runtime` accepts only the Agent LLM bridge role,
   `/internal/llm` accepts only the Platform Analyzer, Evolution, and Judge
   roles, and public `/v1/*` claims are non-authoritative. Evidence must contain
   booleans and counts only, never credentials or derived verifiers.
8. Record boundary B after token-free authenticated traffic. Wait until both the
   global and hierarchy projection offsets reach boundary B and reconcile the
   coverage metadata again.
9. Enable `SENTINEL_LLM_USAGE_V2_ENABLED=true` only after steps 1 through 8
   pass. Restart only the authorized daemon instance and re-check the
   exactly-one-daemon invariant.
10. Send one local-loop request for each hierarchy tier. Reconcile the gateway
   response, v2 event, independent offset, hierarchy aggregate, dashboard API,
   runtime stats, and Prometheus dimensions at one recorded boundary C.
11. Confirm advancing simulation ticks and the absence of panic, drift,
    unbounded retry, crash loop, or unexpected restart before ending the
    window.

## Pass conditions

- Every v2 event has a model/pricing `tier`, `hierarchy_tier` in `1..3`, a
  closed-enum `cost_source`, non-negative finite cost, and the gateway-resolved
  effective model.
- Existing `cost_by_tier` semantics and totals are unchanged.
- The sum of hierarchy aggregate call counts equals attributed v2 coverage at
  the recorded boundary.
- Legacy coverage is explicit and never assigned a guessed hierarchy tier.
- Replaying from a stale hierarchy offset is idempotent.
- The additive API reports both offsets and coverage without requiring a
  `CostView` change.

## Rollback order

1. Disable `SENTINEL_LLM_USAGE_V2_ENABLED` and verify the producer is no longer
   appending v2 events.
2. Record the final v2 event boundary and advance the new projection through
   that boundary before changing binaries.
3. Roll back caller or gateway changes only if the old version still preserves
   the authenticated-path contract. Never restore an unauthenticated internal
   endpoint as a shortcut.
4. The additive table and independent offset may remain in place. Do not reset
   the unrelated global offset and do not delete v2 events.
5. If the old projection binary cannot consume the new table, stop that binary
   and preserve the read model for forward recovery. A rollback must not leave
   v2 events stranded behind an already advanced global offset.
6. Restore the exact recorded binaries/configs, then verify health, restart
   counts, event continuity, both offsets, and aggregate totals.

Any mismatch in authorization, boundary reconciliation, catalog digest, event
shape, offsets, totals, or service stability fails the cutover and keeps the
issue unverified.
