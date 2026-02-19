package messaging

import (
	"fmt"
	"testing"
)

// BenchmarkBuildEventSubject measures subject construction throughput.
func BenchmarkBuildEventSubject(b *testing.B) {
	for range b.N {
		_ = BuildEventSubject("agent_action_received", "AGENT-07")
	}
}

// BenchmarkBuildAlertSubject measures alert subject construction.
func BenchmarkBuildAlertSubject(b *testing.B) {
	for range b.N {
		_ = BuildAlertSubject("AGENT-07")
	}
}

// BenchmarkParseEventSubject measures subject parsing throughput.
func BenchmarkParseEventSubject(b *testing.B) {
	for range b.N {
		_, _, _ = ParseEventSubject("sentinel.events.agent_action_received.AGENT-07")
	}
}

// BenchmarkBuildEventSubject_15Agents simulates subject construction for a full shift.
func BenchmarkBuildEventSubject_15Agents(b *testing.B) {
	types := []string{"agent_action_received", "agent_chat", "bio_state_updated"}

	b.ResetTimer()
	for range b.N {
		for i := 1; i <= 15; i++ {
			for _, t := range types {
				_ = BuildEventSubject(t, fmt.Sprintf("AGENT-%02d", i))
			}
		}
	}
}
