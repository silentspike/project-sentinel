package ticksync

import (
	"fmt"
	"io"
	"log/slog"
	"net/http/httptest"
	"testing"
	"time"
)

type benchResponse struct {
	Content   string `json:"content"`
	RequestID string `json:"request_id"`
}

func BenchmarkHoldAndFlushSingle(b *testing.B) {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	buffer := NewBuffer(time.Second, false, logger)
	defer buffer.Stop()

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		done := buffer.Hold(
			1,
			i%54,
			1,
			fmt.Sprintf("req-%d", i),
			benchResponse{Content: "ok", RequestID: "req"},
			httptest.NewRecorder(),
		)

		buffer.mu.Lock()
		groups := buffer.pending
		buffer.pending = make(map[uint64][]*Entry)
		buffer.mu.Unlock()

		buffer.flushEntries(groups, 1)
		if err := <-done; err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkFlushEntriesBatch10(b *testing.B) {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	buffer := NewBuffer(time.Second, false, logger)
	defer buffer.Stop()

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		entries := make([]*Entry, 0, 10)
		for j := 0; j < 10; j++ {
			entries = append(entries, &Entry{
				AgentID:   j,
				RequestID: fmt.Sprintf("req-%d-%d", i, j),
				Priority:  j % 4,
				Response:  benchResponse{Content: "ok", RequestID: "req"},
				Writer:    httptest.NewRecorder(),
				HeldAt:    time.Now(),
				Done:      make(chan error, 1),
			})
		}

		buffer.flushEntries(map[uint64][]*Entry{1: entries}, 1)
		for _, entry := range entries {
			if err := <-entry.Done; err != nil {
				b.Fatal(err)
			}
		}
	}
}
