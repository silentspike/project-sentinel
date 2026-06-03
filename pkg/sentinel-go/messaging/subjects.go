package messaging

import (
	"fmt"
	"strings"
)

// Subject prefixes.
const (
	SubjectEventsPrefix = "sentinel.events"
	SubjectJudgePrefix  = "sentinel.judge"
	SubjectEBPFPrefix   = "sentinel.ebpf"
)

// sanitizeToken makes an arbitrary string safe as a single NATS subject token.
// NATS rejects subjects containing spaces, or the reserved characters '.' (level
// separator), '*' and '>' (wildcards). Aggregate IDs derived from agent display
// names (e.g. "Michael Hartmann") would otherwise produce an invalid subject and
// block the outbox indefinitely (#475). Empty input maps to "_" so the token slot
// is never empty.
func sanitizeToken(s string) string {
	if s == "" {
		return "_"
	}
	return strings.Map(func(r rune) rune {
		switch r {
		case ' ', '\t', '\n', '\r', '.', '*', '>':
			return '_'
		default:
			return r
		}
	}, s)
}

// BuildEventSubject creates a NATS subject for a domain event.
// Format: sentinel.events.{event_type}.{agent_id}
// Example: sentinel.events.agent_action_received.AGENT-07
// Tokens are sanitized so display-name aggregate IDs cannot break the subject (#475).
func BuildEventSubject(eventType, agentID string) string {
	return fmt.Sprintf("%s.%s.%s", SubjectEventsPrefix, sanitizeToken(eventType), sanitizeToken(agentID))
}

// BuildAlertSubject creates a NATS subject for a judge alert.
// Format: sentinel.judge.alert.{agent_id}
func BuildAlertSubject(agentID string) string {
	return fmt.Sprintf("%s.alert.%s", SubjectJudgePrefix, sanitizeToken(agentID))
}

// BuildEBPFSubject creates a NATS subject for an eBPF metric type.
// Format: sentinel.ebpf.{metric_type}
// Example: sentinel.ebpf.agent-health
// Metric types: agent-health, io-profile, network, psi, status
func BuildEBPFSubject(metricType string) string {
	return fmt.Sprintf("%s.%s", SubjectEBPFPrefix, metricType)
}

// ParseEventSubject extracts event_type and agent_id from a NATS subject.
// Returns ("", "", error) if the subject doesn't match the expected format.
func ParseEventSubject(subject string) (eventType, agentID string, err error) {
	// sentinel.events.{type}.{agent}
	parts := strings.SplitN(subject, ".", 4)
	if len(parts) != 4 || parts[0] != "sentinel" || parts[1] != "events" {
		return "", "", fmt.Errorf("invalid event subject: %s", subject)
	}
	return parts[2], parts[3], nil
}
