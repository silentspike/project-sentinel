package apicp

import (
	"encoding/json"
	"fmt"
	"testing"
)

func BenchmarkObserverRecord(b *testing.B) {
	observer := NewObserver(Config{}, nil)
	defer observer.Stop()

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		observer.Record(
			fmt.Sprintf("fp-%d", i%128),
			fmt.Sprintf("AGENT-%02d", i%54),
			"stabile antwort",
			false,
		)
	}
}

func BenchmarkLearnedPatternLookup(b *testing.B) {
	observer := NewObserver(Config{}, nil)
	defer observer.Stop()

	for i := 0; i < 60; i++ {
		observer.Record("fp-promoted", "AGENT-01", "immer gleich", false)
	}

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		pattern, ok := observer.LearnedPatternFor("AGENT-01", "fp-promoted")
		if !ok || pattern.Content == "" {
			b.Fatal("expected promoted learned pattern")
		}
	}
}

func BenchmarkSnapshotMarshal(b *testing.B) {
	observer := NewObserver(Config{}, nil)
	defer observer.Stop()

	for i := 0; i < 128; i++ {
		agentID := fmt.Sprintf("AGENT-%02d", i%54)
		fp := fmt.Sprintf("fp-%03d", i)
		for sample := 0; sample < 60; sample++ {
			observer.Record(fp, agentID, "stabile antwort", false)
		}
	}

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		snapshot := observer.Snapshot()
		data, err := json.Marshal(snapshot)
		if err != nil {
			b.Fatal(err)
		}
		if len(data) == 0 {
			b.Fatal("expected non-empty snapshot payload")
		}
	}
}
