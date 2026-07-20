# Hierarchy Tier Usage v2 Cutover

This runbook defines the fail-closed deployment order for issue #395. It does
not authorize a deployment, a service restart, a provider call, or a runtime
target. Gate C and a current Gate B reservation are prerequisites.

## Preconditions

- Gate C pins `config/cortex-gateway.toml` by Git blob OID, file SHA-256, and
  `cortex-catalog-v1` semantic digest. The normalized digest input includes
  provider ID/type, default, the complete allowlist, and all three
  `hierarchy_models` mappings. Reassigning a hierarchy tier therefore changes
  the semantic digest and rejects every attestation pinned to the previous
  digest.
- The current Gate C candidate pins are:
  - Git blob OID: `4a575661a99182eabeb67edd34dd277fb9485e32`
  - file SHA-256: `1138e60eaee2fb022394de46c3b49e8c43c509f3d69645c65357f2c82d7a78da`
  - `cortex-catalog-v1` semantic digest:
    `10ed8408bd69c9b10acda44f4cebc889680435945b08a5c3ef2cf068a58680aa`
  - 60-agent matrix SHA-256:
    `a297f22b7c9c32fee18a9f450f12cf52ccef97bd2fcb68e68401b35ea76f6cb5`
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
6. Satisfy the active-provider activation gate without making a provider call:
   - For Ollama, record the exact `name`, `model`, and non-empty content `digest`
     values returned by its token-free inventory. The model IDs must exactly equal
     the immutable catalog allowlist, and `/ready` must report
     `model_inventory_status=validated`. Missing, additional, duplicate,
     digest-less, or unreachable inventory fails readiness. Do not pull or replace
     a model unless the current Gate B mutation scope explicitly allows it.
   - `local-loop` is the only provider without inventory that is intrinsically
     token-free; `/ready` reports `model_inventory_status=token_free_local`.
   - A provider without a token-free inventory contract, including `claude-code`,
     remains blocked both in `/ready` and immediately before provider execution.
     Gate B must materialize the exact public attestation
     `gate-b:<provider-id>:<cortex-catalog-v1 semantic digest>` as
     `CORTEX_MODEL_CATALOG_GATE_B_ATTESTATION`. A stale catalog digest or different
     provider keeps activation fail-closed. This attestation does not authorize a
     real provider call or any spend.
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
    window. Completed gateway responses must enter `llm_completion_outbox`
    before the usage append. Verify the finite append-attempt limit and that a
    terminal `failed` row is quarantined instead of requeued to the provider.
    The bridge reserves `request_id` plus request digest immediately before the
    network call. A crash while that call is ambiguous leaves a
    `provider_in_flight` record and fails closed instead of issuing the request
    again. The reservation persists the canonical `nano:AGENT-NN` owner scope;
    every later enqueue, usage transition, failure, action claim, completion,
    and operator resolution uses that same scope. World ownership neither
    authorizes nor blocks an agent completion. A non-owner receives `NotOwner`.
12. Verify the bounded terminal lifecycle. `enqueue_llm_completion` may only
    transition an existing `provider_in_flight` row to `pending_usage` when its
    request ID, request digest, and persisted owner scope still match. It never
    creates an unreserved row. Automatic recovery has a finite attempt limit.
    `provider_in_flight`, `failed`, and `action_claimed` remain fail-closed until
    an authenticated operator resolves their exact request ID and digest through
    `POST /operator/llm-completions/resolve`. Resolution is owner-fenced, deletes
    the full response row, and writes the compact append-only
    `llm_resolution_<request_id>` idempotency marker. The marker blocks future
    provider replay without retaining the provider payload. New reservations
    fail closed once 10,000 unresolved terminal rows exist; clear the backlog by
    resolving reviewed rows, never by deleting the table.

## Pass conditions

- Every v2 event has a model/pricing `tier`, `hierarchy_tier` in `1..3`, a
  closed-enum `cost_source`, non-negative finite cost, and the gateway-resolved
  `effective_model` persisted in the event payload.
- Existing `cost_by_tier` semantics and totals are unchanged.
- The sum of hierarchy aggregate call counts equals attributed v2 coverage at
  the recorded boundary.
- Legacy coverage is explicit and never assigned a guessed hierarchy tier.
- Replaying from a stale hierarchy offset is idempotent.
- The additive API reports both offsets and coverage without requiring a
  `CostView` change.
- Restarting after a completed provider response but before its usage append
  produces one provider call, two local append attempts, one usage event, and
  one action. Actions are claimed durably before channel delivery. A crash after
  that claim is deliberately fail-closed/at-most-once: the action is not replayed
  automatically and the `action_claimed` row remains for operator diagnosis and
  explicit authenticated resolution.
- With World remotely owned and `AGENT-07` locally Owner/Routable, the complete
  reservation-to-completion recovery path succeeds for `AGENT-07`. The same
  operation for a foreign agent fails with `NotOwner`.

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
