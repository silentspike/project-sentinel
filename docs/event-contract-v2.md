# Event Contract V2

Project Sentinel treats a committed business event as authority, not as a log
message. `AppendProposalV2` is the caller's authenticated request;
`EventEnvelopeV2` is the store-sealed result. Callers cannot choose the event
truth generation, stream revision, global position, append time, receipt digest,
or envelope digest.

## Authority Boundary

`EventAppendGateway` is the only V2 authority writer. It performs these steps in
one fenced SQLite transaction:

1. validate the schema, codec, durability, producer, payload digest, and bounded
   `CausalContextV1`, including an exact optional tick binding;
2. bind the authenticated caller to the proposal's producer and authority scope;
3. resolve an exact scoped operation replay before checking the current head;
4. reject another request digest or an incorrect expected stream revision;
5. assign UUIDv7 identity, generation-local position, revision, and append time;
6. insert the event, delivery intents, local effect reservations, operation
   outcome, stream head, and next position atomically;
7. return the sealed envelope and outcome digest.

An exact retry returns the original envelope with
`ReplayOfPriorOperation`. It does not create a second event or authorize a
second external effect. V1 rows remain readable as
`UnknownV1NonAuthorizing`; the compatibility reader never invents owner,
generation, schema, durability, or causal authority.

## Storage Contract

The daemon is the sole live-store migration executor. Its offline database
maintenance and replay tools may create only isolated output or scratch stores.
The ordered migration is
`crates/sentinel-limbo/migrations/event-store/0001-event-envelope-v2.sql` with
SHA-256
`472b60a6cd218422b946f03e01e50d3566b563759899a027c02c047519097e86`.
It creates the V2 event, operation, stream-head, delivery-intent, effect-
reservation, and truth-metadata tables as one migration transaction.

Projection, Night Run, NATS bridge, and Cortex open the database through a
compatible-participant path. That path creates no schema, verifies the migration
name and checksum, verifies every required table and truth-metadata row, and
verifies every required V2 column before exposing the handle. It uses
connection-local `synchronous=FULL` and foreign keys, and requires the daemon-set
`journal_mode=WAL` on readback without changing persistent SQLite settings. A
missing path, partial schema, caller-controlled SQLite URI, or non-WAL store is
rejected without creating or repairing a database. Cortex receives
only the named `CortexAudit` V1 compatibility capability; the NATS bridge does
not receive an event-append capability. Startup fails
closed when the daemon has not installed the exact schema.

Authoritative and rebuildable records share the strict connection policy for
the single-node architecture. This avoids a second acknowledgement path and a
false durability distinction inside one file. Rebuildable telemetry still has
to name and prove its source before it can be regenerated.

## Schema Registry

Every V2 event type is registered with an exact schema version, codec,
durability class, allowed producer set, payload validator, root-or-direct
causation policy, and optional deterministic upcast edge. Registry manifests
are canonical and digestible. A root family rejects a direct cause; every
non-root family requires an already committed direct event from the same tenant,
company, project, and event-truth generation before append. An invented,
cross-project, future, or stale-generation cause is rejected transactionally.
Upcasting returns derived bytes plus the source/target versions and applied
upcaster identities; it never rewrites stored payloads. An unknown event type,
unsupported known version, broken upcast chain, wrong producer, or wrong
durability fails before the append transaction.

The S0 inference registry owns eleven deterministic-CBOR event records. Budget,
admission, dispatch, outcome, and usage facts are `Authoritative`. Port requests
and measured provider capabilities are `DurableOperational`. The Rust C0
authority is the only producer of inference authority records; Cortex may
produce only authenticated port requests and capability observations.

`schemas/event/v2/golden-vectors.json` is the shared Rust/Go contract fixture.
It follows one stable request from customer admission through project creation,
work claim, artifact commit, independent QA approval, and delivery acceptance.
Every step binds the same authority scope and request identity, the prior event
as its direct cause, canonical proposal/context/envelope bytes, and exact
SHA-256 digests. Mutation tests reject a missing or invented cause, payload
changes, and unknown fields before an event or effect can be committed.

## Cross-Language Inference Contract

Rust and Go implement the same closed deterministic-CBOR value algebra. It has
no null, negative integer, float, tag, indefinite value, duplicate key, unknown
field, or implicit normalization. Text must already be NFC. Digest fields are
exactly 32 bytes, timestamps are unsigned Unix milliseconds, enums are closed,
and optional values are represented only by field absence.

`schemas/inference/v1/golden-vectors.json` covers every one of the eleven S0
records. `schemas/inference/v1/control-vectors.json` additionally covers all
eleven legal reservation transitions, all six authority-port methods, and all
ten typed authority responses. Only a fresh `COMMITTED` response to
`BEGIN_DISPATCH` may set `provider_io_authorized=true`; replay and every failure
result remain non-authorizing.

## Legacy Producer Inventory

The raw V1 methods are crate-private. Every remaining compatibility callsite
must select one named `LegacyEventProducer`; the repository boundary checker
rejects an unclassified call or a second production DDL owner. This inventory
does not promote V1 rows to authority:

| Producer | Existing information | V2 target class |
|---|---|---|
| `EcsTickBatch` | ECS actions and periodic tick snapshots | durable operational; snapshots are rebuildable only from an accepted source cut |
| `GaiaReadiness` | readiness alerts and cursor observations | durable operational |
| `NightRun` | consolidation and maintenance outcomes | durable operational |
| `RuntimeAgent` | runtime lifecycle observations | durable operational |
| `DaemonOrchestrator` | lifecycle, restore, shift, and security events | event-specific; authoritative transitions require V2 authority context |
| `DaemonOperatorApi` | authenticated operator/security actions | authoritative when they mutate policy or work state |
| `DaemonWorkflow` | customer, project, delivery, and Gaia lineage | authoritative |
| `DaemonWorkbench` | invocation and artifact lineage | authoritative |
| `PlatformControlPlane` | repair decisions and control actions | authoritative decision plus durable operational diagnostics |
| `ResourceManager` | pressure and resource-control actions | durable operational |
| `TestHarness`, `BenchmarkHarness` | non-production fixtures | no production authority |

New authority producers may not use this compatibility path. Existing V1
callers remain visibly quarantined until their owning domain supplies the
authenticated tenant/company/project context required for a V2 proposal.

## Failure Model

The append tests cover exact replay, conflicting request digest, lost-update
races, cross-scope rebinding, every transaction insert/update boundary, and two
real child-process cuts: immediately before commit and immediately after commit
but before the caller receives a result. The former exposes no row; the latter
reopens as one committed event and returns the original outcome on retry.

SQLite `FULL` synchronization is the acknowledgement policy. Live acceptance
also verifies WAL/checkpoint recovery, read-only/I/O failure behavior, migration
checksum rejection, restart replay, and bounded WAL growth on `.240`. A process
test proves the software crash cut; it does not claim survival of storage-device
cache loss beyond SQLite and the host's durable-write guarantees.

## Rollout And Rollback

Readers and migration verification deploy before any V2 producer. After every
consumer accepts both formats, producers activate one registered event family
at a time. Rollback stops V2 production and restores the prior binary/config,
but retains the migration and V2 decoder after the first V2 row exists. No
rollback rewrites or deletes committed events.
