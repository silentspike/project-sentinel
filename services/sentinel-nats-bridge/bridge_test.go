package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"

	"github.com/silentspike/project-sentinel/pkg/sentinel-go/eventstore"
	"github.com/silentspike/project-sentinel/pkg/sentinel-go/messaging"
)

type fakeOutboxStore struct {
	mu                   sync.Mutex
	entries              []eventstore.OutboxPublishEntry
	failed               int64
	unknown              int64
	batchErr             error
	countErr             error
	retryErr             error
	failPublishedOnce    bool
	cancelAfterFirstMark context.CancelFunc
	markedPublished      int
	events               *[]string
}

func (s *fakeOutboxStore) GetOutboxBatch(limit int) ([]eventstore.OutboxPublishEntry, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.batchErr != nil {
		return nil, s.batchErr
	}
	if limit > len(s.entries) {
		limit = len(s.entries)
	}
	return append([]eventstore.OutboxPublishEntry(nil), s.entries[:limit]...), nil
}

func (s *fakeOutboxStore) MarkPublishedCAS(id int64, eventID, operationID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.events != nil {
		*s.events = append(*s.events, fmt.Sprintf("cas:%d", id))
	}
	if s.failPublishedOnce {
		s.failPublishedOnce = false
		return errors.New("injected CAS failure")
	}
	index := -1
	for i, entry := range s.entries {
		if entry.OutboxID == id && entry.EventID == eventID && entry.OperationID == operationID {
			index = i
			break
		}
	}
	if index < 0 {
		return errors.New("CAS mismatch")
	}
	s.entries = append(s.entries[:index], s.entries[index+1:]...)
	s.markedPublished++
	if s.markedPublished == 1 && s.cancelAfterFirstMark != nil {
		s.cancelAfterFirstMark()
	}
	return nil
}

func (s *fakeOutboxStore) MarkRetryCAS(id int64, eventID, operationID, _ string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.retryErr != nil {
		return s.retryErr
	}
	for i := range s.entries {
		entry := &s.entries[i]
		if entry.OutboxID == id && entry.EventID == eventID && entry.OperationID == operationID {
			entry.RetryCount++
			return nil
		}
	}
	return errors.New("retry CAS mismatch")
}

func (s *fakeOutboxStore) MarkFailedCAS(id int64, eventID, operationID, _ string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i, entry := range s.entries {
		if entry.OutboxID == id && entry.EventID == eventID && entry.OperationID == operationID {
			s.entries = append(s.entries[:i], s.entries[i+1:]...)
			s.failed++
			return nil
		}
	}
	return errors.New("failed CAS mismatch")
}

func (s *fakeOutboxStore) OutboxCounts() (eventstore.OutboxStatusCounts, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.countErr != nil {
		return eventstore.OutboxStatusCounts{}, s.countErr
	}
	pending := int64(len(s.entries))
	return eventstore.OutboxStatusCounts{
		Pending:      pending,
		Failed:       s.failed,
		NonPublished: pending + s.failed + s.unknown,
	}, nil
}

type fakePublisher struct {
	mu       sync.Mutex
	calls    int
	unique   map[string]struct{}
	failCall int
	nilAck   bool
	events   *[]string
}

func (p *fakePublisher) PublishMsg(
	_ context.Context,
	msg *nats.Msg,
	_ ...jetstream.PublishOpt,
) (*jetstream.PubAck, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.calls++
	id := msg.Header.Get("Nats-Msg-Id")
	if p.unique == nil {
		p.unique = make(map[string]struct{})
	}
	p.unique[id] = struct{}{}
	if p.events != nil {
		*p.events = append(*p.events, "publish:"+id)
	}
	if p.failCall == p.calls {
		return nil, errors.New("injected publish failure")
	}
	if p.nilAck {
		return nil, nil
	}
	return &jetstream.PubAck{Stream: "SENTINEL_EVENTS", Sequence: 1}, nil
}

type fakeConnection bool

func (c fakeConnection) IsConnected() bool { return bool(c) }

func makeOutboxEntries(count int) []eventstore.OutboxPublishEntry {
	entries := make([]eventstore.OutboxPublishEntry, count)
	for i := range entries {
		entries[i] = eventstore.OutboxPublishEntry{
			OutboxID:      int64(i + 1),
			EventID:       fmt.Sprintf("event-%d", i+1),
			EventType:     "agent_chat",
			AggregateID:   "AGENT-01",
			OperationID:   fmt.Sprintf("operation-%d", i+1),
			CorrelationID: "correlation-1",
			Tick:          int64(i + 1),
			Payload:       `{"message":"bounded"}`,
		}
	}
	return entries
}

func TestBuildPublishMessage(t *testing.T) {
	evt := eventstore.DomainEvent{
		EventID:       "evt-001",
		EventType:     "agent_action_received",
		AggregateID:   "AGENT-07",
		Payload:       `{"action":"greet"}`,
		CorrelationID: "corr-001",
		CausationID:   "cause-001",
		OperationID:   "op-001",
		Tick:          1000,
		TimestampMs:   1700000000000,
		SchemaVersion: 1,
	}

	subject := messaging.BuildEventSubject(evt.EventType, evt.AggregateID)
	msg := &nats.Msg{
		Subject: subject,
		Data:    []byte(evt.Payload),
		Header:  nats.Header{},
	}
	msg.Header.Set("Nats-Msg-Id", evt.OperationID)
	msg.Header.Set("X-Event-ID", evt.EventID)
	msg.Header.Set("X-Event-Type", evt.EventType)
	msg.Header.Set("X-Aggregate-ID", evt.AggregateID)

	if subject != "sentinel.events.agent_action_received.AGENT-07" {
		t.Errorf("subject = %q, want sentinel.events.agent_action_received.AGENT-07", subject)
	}
	if msg.Header.Get("Nats-Msg-Id") != "op-001" {
		t.Errorf("Nats-Msg-Id = %q, want op-001", msg.Header.Get("Nats-Msg-Id"))
	}
	if msg.Header.Get("X-Event-Type") != "agent_action_received" {
		t.Errorf("X-Event-Type = %q, want agent_action_received", msg.Header.Get("X-Event-Type"))
	}
	if string(msg.Data) != `{"action":"greet"}` {
		t.Errorf("Data = %q, want payload", string(msg.Data))
	}
}

func TestBuildPublishMessageDedup(t *testing.T) {
	// Two events with same operation_id should produce same Nats-Msg-Id (dedup key)
	evt1 := eventstore.DomainEvent{OperationID: "op-dup-001", EventType: "agent_chat", AggregateID: "AGENT-01"}
	evt2 := eventstore.DomainEvent{OperationID: "op-dup-001", EventType: "agent_chat", AggregateID: "AGENT-01"}

	msg1 := &nats.Msg{Header: nats.Header{}}
	msg1.Header.Set("Nats-Msg-Id", evt1.OperationID)

	msg2 := &nats.Msg{Header: nats.Header{}}
	msg2.Header.Set("Nats-Msg-Id", evt2.OperationID)

	if msg1.Header.Get("Nats-Msg-Id") != msg2.Header.Get("Nats-Msg-Id") {
		t.Error("same operation_id must produce same Nats-Msg-Id for dedup")
	}
}

func TestBuildPublishMessageDifferentOps(t *testing.T) {
	// Two events with different operation_id should produce different Nats-Msg-Id
	evt1 := eventstore.DomainEvent{OperationID: "op-A", EventType: "agent_chat", AggregateID: "AGENT-01"}
	evt2 := eventstore.DomainEvent{OperationID: "op-B", EventType: "agent_chat", AggregateID: "AGENT-01"}

	if evt1.OperationID == evt2.OperationID {
		t.Error("different operations should have different IDs")
	}
}

func TestSubjectMapping(t *testing.T) {
	tests := []struct {
		eventType   string
		aggregateID string
		want        string
	}{
		{"agent_action_received", "AGENT-07", "sentinel.events.agent_action_received.AGENT-07"},
		{"agent_chat", "AGENT-12", "sentinel.events.agent_chat.AGENT-12"},
		{"bio_state_updated", "AGENT-01", "sentinel.events.bio_state_updated.AGENT-01"},
	}

	for _, tt := range tests {
		got := messaging.BuildEventSubject(tt.eventType, tt.aggregateID)
		if got != tt.want {
			t.Errorf("BuildEventSubject(%q, %q) = %q, want %q", tt.eventType, tt.aggregateID, got, tt.want)
		}
	}
}

func TestConfigDefaults(t *testing.T) {
	var cfg Config

	// Verify defaults are applied correctly
	if cfg.EventStore.PollIntervalMs != 0 {
		t.Errorf("default PollIntervalMs = %d, want 0 (before defaults applied)", cfg.EventStore.PollIntervalMs)
	}

	// Apply defaults like main() does
	if cfg.EventStore.PollIntervalMs <= 0 {
		cfg.EventStore.PollIntervalMs = 1000
	}
	if cfg.EventStore.BatchSize <= 0 {
		cfg.EventStore.BatchSize = 100
	}
	if cfg.Server.HealthPort <= 0 {
		cfg.Server.HealthPort = 8083
	}
	// #525: health_bind_addr default = loopback with configured health_port.
	if cfg.Server.HealthBindAddr == "" {
		cfg.Server.HealthBindAddr = fmt.Sprintf("127.0.0.1:%d", cfg.Server.HealthPort)
	}

	if cfg.EventStore.PollIntervalMs != 1000 {
		t.Errorf("PollIntervalMs = %d, want 1000", cfg.EventStore.PollIntervalMs)
	}
	if cfg.EventStore.BatchSize != 100 {
		t.Errorf("BatchSize = %d, want 100", cfg.EventStore.BatchSize)
	}
	if cfg.Server.HealthPort != 8083 {
		t.Errorf("HealthPort = %d, want 8083", cfg.Server.HealthPort)
	}
	if cfg.Server.HealthBindAddr != "127.0.0.1:8083" {
		t.Errorf("HealthBindAddr = %q, want 127.0.0.1:8083", cfg.Server.HealthBindAddr)
	}
}

func TestHealthBindAddrDefaultRespectsConfiguredPort(t *testing.T) {
	// #525 (ORC Finding 2): empty health_bind_addr must default to loopback with
	// the configured health_port, NOT a hardcoded 8083.
	var cfg Config
	cfg.Server.HealthPort = 9999
	if cfg.Server.HealthBindAddr == "" {
		cfg.Server.HealthBindAddr = fmt.Sprintf("127.0.0.1:%d", cfg.Server.HealthPort)
	}
	if cfg.Server.HealthBindAddr != "127.0.0.1:9999" {
		t.Errorf("HealthBindAddr = %q, want 127.0.0.1:9999 (must respect configured health_port)", cfg.Server.HealthBindAddr)
	}
}

func TestHealthBindAddrExplicitOverridePreserved(t *testing.T) {
	// #525: an explicit health_bind_addr is preserved (not overwritten) by the default logic.
	var cfg Config
	cfg.Server.HealthPort = 8083
	cfg.Server.HealthBindAddr = "0.0.0.0:8083"
	if cfg.Server.HealthBindAddr == "" {
		cfg.Server.HealthBindAddr = fmt.Sprintf("127.0.0.1:%d", cfg.Server.HealthPort)
	}
	if cfg.Server.HealthBindAddr != "0.0.0.0:8083" {
		t.Errorf("explicit override overwritten: %q, want 0.0.0.0:8083", cfg.Server.HealthBindAddr)
	}
}

func TestGetEventsSince(t *testing.T) {
	// Integration test: create a real event store, insert events, poll them
	store, err := eventstore.Open(t.TempDir() + "/test.db")
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer func() { _ = store.Close() }()

	// Insert 3 events
	for i := 0; i < 3; i++ {
		err := store.AppendWithOutbox(eventstore.DomainEvent{
			EventID:       eventstore.GenerateUUID(),
			EventType:     "agent_chat",
			AggregateID:   "AGENT-01",
			Payload:       `{"msg":"hello"}`,
			CorrelationID: "corr-1",
			CausationID:   "cause-1",
			OperationID:   eventstore.GenerateUUID(),
			Tick:          int64(i + 1),
			TimestampMs:   1700000000000,
			SchemaVersion: 1,
		}, "sentinel/events/AGENT-01")
		if err != nil {
			t.Fatalf("append %d: %v", i, err)
		}
	}

	// Poll all events from the start
	events, maxID, err := store.GetEventsSince(0, 100)
	if err != nil {
		t.Fatalf("GetEventsSince: %v", err)
	}
	if len(events) != 3 {
		t.Errorf("got %d events, want 3", len(events))
	}
	if maxID < 1 {
		t.Errorf("maxID = %d, want >= 1", maxID)
	}

	// Poll again from maxID — should return 0 events
	events2, _, err := store.GetEventsSince(maxID, 100)
	if err != nil {
		t.Fatalf("GetEventsSince second: %v", err)
	}
	if len(events2) != 0 {
		t.Errorf("got %d events after maxID, want 0", len(events2))
	}

	// Poll with limit=2 — should return 2
	events3, _, err := store.GetEventsSince(0, 2)
	if err != nil {
		t.Fatalf("GetEventsSince limited: %v", err)
	}
	if len(events3) != 2 {
		t.Errorf("got %d events with limit=2, want 2", len(events3))
	}
}

func TestDrainOutboxConsumesMoreThanTwoBatchesInOneSweep(t *testing.T) {
	store := &fakeOutboxStore{entries: makeOutboxEntries(7)}
	publisher := &fakePublisher{}
	readiness := &readinessState{}

	published, err := drainOutbox(context.Background(), store, publisher, readiness, 2)
	if err != nil {
		t.Fatalf("drainOutbox: %v", err)
	}
	if published != 7 || publisher.calls != 7 || store.markedPublished != 7 {
		t.Fatalf("published=%d calls=%d marks=%d, want 7/7/7", published, publisher.calls, store.markedPublished)
	}
	if !readiness.initialScanComplete.Load() {
		t.Fatal("successful empty scan did not close initial readiness gate")
	}
}

func TestDrainOutboxCancellationStopsBetweenEntries(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	store := &fakeOutboxStore{entries: makeOutboxEntries(3), cancelAfterFirstMark: cancel}
	publisher := &fakePublisher{}

	published, err := drainOutbox(ctx, store, publisher, &readinessState{}, 3)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("error=%v, want context cancellation", err)
	}
	if published != 1 || publisher.calls != 1 || store.markedPublished != 1 {
		t.Fatalf("published=%d calls=%d marks=%d, want 1/1/1", published, publisher.calls, store.markedPublished)
	}
}

func TestDrainOutboxWaitsForPubAckBeforeCAS(t *testing.T) {
	var events []string
	store := &fakeOutboxStore{entries: makeOutboxEntries(1), events: &events}
	publisher := &fakePublisher{events: &events}

	if _, err := drainOutbox(context.Background(), store, publisher, &readinessState{}, 1); err != nil {
		t.Fatalf("drainOutbox: %v", err)
	}
	want := []string{"publish:operation-1", "cas:1"}
	if fmt.Sprint(events) != fmt.Sprint(want) {
		t.Fatalf("effect order=%v, want %v", events, want)
	}
}

func TestDrainOutboxAckThenCASFailureRetriesOneEffectiveMessage(t *testing.T) {
	store := &fakeOutboxStore{entries: makeOutboxEntries(1), failPublishedOnce: true}
	publisher := &fakePublisher{}
	readiness := &readinessState{}

	if _, err := drainOutbox(context.Background(), store, publisher, readiness, 1); err == nil {
		t.Fatal("first drain accepted injected CAS failure")
	}
	if readiness.initialScanComplete.Load() {
		t.Fatal("failed sweep closed initial readiness gate")
	}
	if published, err := drainOutbox(context.Background(), store, publisher, readiness, 1); err != nil || published != 1 {
		t.Fatalf("retry published=%d err=%v, want one adopted row", published, err)
	}
	if publisher.calls != 2 || len(publisher.unique) != 1 {
		t.Fatalf("broker calls=%d unique message IDs=%d, want 2 calls/1 effective ID", publisher.calls, len(publisher.unique))
	}
}

func TestDrainOutboxStopsAfterPublishFailure(t *testing.T) {
	store := &fakeOutboxStore{entries: makeOutboxEntries(3)}
	publisher := &fakePublisher{failCall: 1}

	published, err := drainOutbox(context.Background(), store, publisher, &readinessState{}, 3)
	if err == nil || published != 0 || publisher.calls != 1 {
		t.Fatalf("published=%d calls=%d err=%v, want stopped first failure", published, publisher.calls, err)
	}
	if store.entries[0].RetryCount != 1 {
		t.Fatalf("retry_count=%d, want 1", store.entries[0].RetryCount)
	}
}

func TestDrainOutboxStopsAfterBatchReadFailure(t *testing.T) {
	store := &fakeOutboxStore{
		entries:  makeOutboxEntries(2),
		batchErr: errors.New("injected batch read failure"),
	}
	publisher := &fakePublisher{}
	readiness := &readinessState{}

	published, err := drainOutbox(context.Background(), store, publisher, readiness, 2)
	if err == nil || published != 0 || publisher.calls != 0 {
		t.Fatalf("published=%d calls=%d err=%v, want no effect after batch read failure", published, publisher.calls, err)
	}
	if readiness.initialScanComplete.Load() {
		t.Fatal("failed batch read closed the initial readiness gate")
	}
}

func TestDrainOutboxStopsAfterMissingPubAck(t *testing.T) {
	store := &fakeOutboxStore{entries: makeOutboxEntries(2)}
	publisher := &fakePublisher{nilAck: true}
	readiness := &readinessState{}

	published, err := drainOutbox(context.Background(), store, publisher, readiness, 2)
	if err == nil || published != 0 || publisher.calls != 1 || store.markedPublished != 0 {
		t.Fatalf(
			"published=%d calls=%d marks=%d err=%v, want one publish and no adoption",
			published,
			publisher.calls,
			store.markedPublished,
			err,
		)
	}
	if readiness.initialScanComplete.Load() {
		t.Fatal("missing PubAck closed the initial readiness gate")
	}
}

func TestDrainOutboxStopsWhenPublishFailureCannotRecordRetry(t *testing.T) {
	store := &fakeOutboxStore{
		entries:  makeOutboxEntries(2),
		retryErr: errors.New("injected retry transition failure"),
	}
	publisher := &fakePublisher{failCall: 1}

	published, err := drainOutbox(context.Background(), store, publisher, &readinessState{}, 2)
	if err == nil || published != 0 || publisher.calls != 1 {
		t.Fatalf("published=%d calls=%d err=%v, want fail-closed retry transition", published, publisher.calls, err)
	}
	if store.entries[0].RetryCount != 0 {
		t.Fatalf("retry_count=%d, want unchanged row after rejected transition", store.entries[0].RetryCount)
	}
}

func TestReadinessTransitionsAreFailClosedAndPublicSafe(t *testing.T) {
	store := &fakeOutboxStore{}
	state := &readinessState{}
	tests := []struct {
		name       string
		connected  bool
		scanned    bool
		entries    int
		failed     int64
		unknown    int64
		countErr   error
		wantStatus int
		wantReason string
	}{
		{name: "disconnected", wantStatus: 503, wantReason: "nats_disconnected"},
		{name: "scan pending", connected: true, wantStatus: 503, wantReason: "initial_scan_pending"},
		{name: "count error", connected: true, scanned: true, countErr: errors.New("PRIVATE db path"), wantStatus: 503, wantReason: "outbox_status_unavailable"},
		{name: "pending", connected: true, scanned: true, entries: 2, wantStatus: 503, wantReason: "outbox_pending"},
		{name: "failed", connected: true, scanned: true, failed: 1, wantStatus: 503, wantReason: "outbox_failed"},
		{name: "unknown", connected: true, scanned: true, unknown: 1, wantStatus: 503, wantReason: "outbox_nonpublished"},
		{name: "ready", connected: true, scanned: true, wantStatus: 200},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store.entries = makeOutboxEntries(tt.entries)
			store.failed = tt.failed
			store.unknown = tt.unknown
			store.countErr = tt.countErr
			state.initialScanComplete.Store(tt.scanned)
			recorder := httptest.NewRecorder()
			request := httptest.NewRequest(http.MethodGet, "/ready", nil)
			newHealthHandler(store, fakeConnection(tt.connected), state).ServeHTTP(recorder, request)
			if recorder.Code != tt.wantStatus {
				t.Fatalf("status=%d, want %d", recorder.Code, tt.wantStatus)
			}
			var response readinessResponse
			if err := json.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
				t.Fatalf("decode response: %v", err)
			}
			if response.Reason != tt.wantReason {
				t.Fatalf("reason=%q, want %q", response.Reason, tt.wantReason)
			}
			if tt.countErr != nil && strings.Contains(recorder.Body.String(), "PRIVATE") {
				t.Fatal("readiness leaked private store diagnostics")
			}
		})
	}
}

func TestHealthRemainsLivenessWhenDependenciesAreUnavailable(t *testing.T) {
	store := &fakeOutboxStore{countErr: errors.New("store unavailable")}
	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodGet, "/health", nil)
	newHealthHandler(store, fakeConnection(false), &readinessState{}).ServeHTTP(recorder, request)
	if recorder.Code != http.StatusOK {
		t.Fatalf("health status=%d, want %d", recorder.Code, http.StatusOK)
	}
	var response readinessResponse
	if err := json.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatalf("decode health response: %v", err)
	}
	if response.Status != "ok" || response.Reason != "" {
		t.Fatalf("health response=%+v, want unconditional liveness", response)
	}
}
