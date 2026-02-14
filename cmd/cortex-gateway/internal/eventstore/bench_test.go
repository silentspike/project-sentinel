package eventstore

import (
	"fmt"
	"path/filepath"
	"testing"
)

// BenchmarkAppendWithOutbox misst die Latenz eines einzelnen atomaren
// Event+Outbox Writes (Zielwert: <1ms auf Deployment-VM).
func BenchmarkAppendWithOutbox(b *testing.B) {
	dir := b.TempDir()
	store, err := Open(filepath.Join(dir, "bench.db"))
	if err != nil {
		b.Fatal(err)
	}
	defer func() { _ = store.Close() }()

	b.ResetTimer()
	for i := range b.N {
		event := DomainEvent{
			EventID:          GenerateUUID(),
			EventType:        "agent_move",
			AggregateID:      fmt.Sprintf("AGENT-%02d", (i%15)+1),
			Payload:          `{"target":"kueche","emotion":"happy"}`,
			CorrelationID:    fmt.Sprintf("req-%d", i),
			OperationID:      GenerateUUID(),
			Tick:             int64(i),
			SchemaVersion:    1,
			CompensationType: "none",
		}
		if err := store.AppendWithOutbox(event, "sentinel/cortex/events/test"); err != nil {
			b.Fatal(err)
		}
	}
}

// BenchmarkAppendWithOutbox_15Agents simuliert einen Tick mit 15 Agent-Events.
// Misst ob ein vollstaendiger Tick unter 10ms bleibt (>100 ticks/s moeglich).
func BenchmarkAppendWithOutbox_15Agents(b *testing.B) {
	dir := b.TempDir()
	store, err := Open(filepath.Join(dir, "bench_tick.db"))
	if err != nil {
		b.Fatal(err)
	}
	defer func() { _ = store.Close() }()

	b.ResetTimer()
	for tick := range b.N {
		for agent := 1; agent <= 15; agent++ {
			event := DomainEvent{
				EventID:       GenerateUUID(),
				EventType:     "agent_action_received",
				AggregateID:   fmt.Sprintf("AGENT-%02d", agent),
				Payload:       fmt.Sprintf(`{"tick":%d,"action":"work"}`, tick),
				CorrelationID: fmt.Sprintf("corr-tick-%d", tick),
				OperationID:   GenerateUUID(),
				Tick:          int64(tick),
				SchemaVersion: 1,
			}
			if err := store.AppendWithOutbox(event, fmt.Sprintf("sentinel/cortex/events/AGENT-%02d", agent)); err != nil {
				b.Fatal(err)
			}
		}
	}
}

// BenchmarkIdempotentRetry misst die Kosten eines idempotenten Retry
// (INSERT OR IGNORE bei bestehendem operation_id).
func BenchmarkIdempotentRetry(b *testing.B) {
	dir := b.TempDir()
	store, err := Open(filepath.Join(dir, "bench_retry.db"))
	if err != nil {
		b.Fatal(err)
	}
	defer func() { _ = store.Close() }()

	// Pre-insert the event
	event := DomainEvent{
		EventID:       GenerateUUID(),
		EventType:     "agent_chat",
		AggregateID:   "AGENT-01",
		Payload:       `{"msg":"hello"}`,
		CorrelationID: "req-retry",
		OperationID:   "fixed-op-id",
		Tick:          1,
		SchemaVersion: 1,
	}
	if err := store.AppendWithOutbox(event, "test/topic"); err != nil {
		b.Fatal(err)
	}

	b.ResetTimer()
	for range b.N {
		// Same operation_id → INSERT OR IGNORE
		retry := DomainEvent{
			EventID:       GenerateUUID(),
			EventType:     "agent_chat",
			AggregateID:   "AGENT-01",
			Payload:       `{"msg":"hello"}`,
			CorrelationID: "req-retry",
			OperationID:   "fixed-op-id",
			Tick:          1,
			SchemaVersion: 1,
		}
		if err := store.AppendWithOutbox(retry, "test/topic"); err != nil {
			b.Fatal(err)
		}
	}
}
