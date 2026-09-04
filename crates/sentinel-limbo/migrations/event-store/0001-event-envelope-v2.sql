CREATE TABLE event_truth_metadata (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    schema_version INTEGER NOT NULL,
    event_truth_generation INTEGER NOT NULL CHECK (event_truth_generation > 0),
    next_global_position INTEGER NOT NULL CHECK (next_global_position > 0)
);

INSERT INTO event_truth_metadata (
    singleton_id, schema_version, event_truth_generation, next_global_position
) VALUES (1, 2, 1, 1);

CREATE TABLE event_stream_heads_v2 (
    stream_namespace TEXT PRIMARY KEY,
    stream_revision INTEGER NOT NULL CHECK (stream_revision >= 0)
);

CREATE TABLE events_v2 (
    event_id TEXT PRIMARY KEY,
    event_truth_generation INTEGER NOT NULL,
    stream_namespace TEXT NOT NULL,
    stream_revision INTEGER NOT NULL,
    global_position INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    payload_codec TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    payload BLOB NOT NULL,
    causal_context_json TEXT NOT NULL,
    causal_context_digest TEXT NOT NULL,
    producer TEXT NOT NULL,
    owner_term_json TEXT,
    tick INTEGER,
    appended_at_ms INTEGER NOT NULL,
    durability TEXT NOT NULL,
    canonical_request_digest TEXT NOT NULL,
    append_receipt_digest TEXT NOT NULL,
    sealed_envelope_digest TEXT NOT NULL,
    UNIQUE (event_truth_generation, global_position),
    UNIQUE (stream_namespace, stream_revision)
);

CREATE INDEX idx_events_v2_type_position
    ON events_v2 (event_type, event_truth_generation, global_position);

CREATE TABLE event_operations_v2 (
    authority_scope_digest TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    canonical_request_digest TEXT NOT NULL,
    event_id TEXT NOT NULL REFERENCES events_v2(event_id),
    outcome_digest TEXT NOT NULL,
    PRIMARY KEY (authority_scope_digest, operation_id)
);

CREATE TABLE delivery_intents_v2 (
    intent_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES events_v2(event_id),
    authority_scope_digest TEXT NOT NULL,
    causal_context_digest TEXT NOT NULL,
    topic TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'claimed', 'completed', 'quarantined'))
);

CREATE INDEX idx_delivery_intents_v2_pending
    ON delivery_intents_v2 (status, event_id) WHERE status = 'pending';

CREATE TABLE local_effect_reservations_v2 (
    effect_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES events_v2(event_id),
    authority_scope_digest TEXT NOT NULL,
    causal_context_digest TEXT NOT NULL,
    effect_kind TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('reserved', 'executing', 'completed', 'quarantined'))
);
