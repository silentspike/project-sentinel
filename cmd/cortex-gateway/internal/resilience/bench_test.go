package resilience

import (
	"fmt"
	"testing"
	"time"
)

// BenchmarkInFlightTrackAccept measures the full Track+Accept lifecycle (AC-1).
func BenchmarkInFlightTrackAccept(b *testing.B) {
	m := NewInFlightMap(5 * time.Second)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		id := fmt.Sprintf("q-%d", i)
		m.Track(id, int64(i))
		m.Accept(id, int64(i))
	}
}

// BenchmarkInFlightTrackCancel measures the Track+Cancel path (timeout scenario, AC-2).
func BenchmarkInFlightTrackCancel(b *testing.B) {
	m := NewInFlightMap(5 * time.Second)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		id := fmt.Sprintf("q-%d", i)
		m.Track(id, int64(i))
		m.Cancel(id)
	}
}

// BenchmarkInFlightPrune1000 measures pruning 1000 expired entries (AC-N1).
func BenchmarkInFlightPrune1000(b *testing.B) {
	for i := 0; i < b.N; i++ {
		b.StopTimer()
		m := NewInFlightMap(1 * time.Millisecond)
		for j := 0; j < 1000; j++ {
			m.Track(fmt.Sprintf("q-%d", j), int64(j))
		}
		time.Sleep(2 * time.Millisecond)
		b.StartTimer()
		m.Prune()
	}
}

// BenchmarkInFlightConcurrent measures parallel Track+Accept under contention (AC-N1).
func BenchmarkInFlightConcurrent(b *testing.B) {
	m := NewInFlightMap(5 * time.Second)
	b.RunParallel(func(pb *testing.PB) {
		i := 0
		for pb.Next() {
			id := fmt.Sprintf("q-%d-%d", time.Now().UnixNano(), i)
			m.Track(id, int64(i))
			m.Accept(id, int64(i))
			i++
		}
	})
}
