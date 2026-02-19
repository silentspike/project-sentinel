package messaging

import (
	"fmt"
	"strings"
)

// Subject prefixes.
const (
	SubjectEventsPrefix = "sentinel.events"
	SubjectJudgePrefix  = "sentinel.judge"
)

// BuildEventSubject creates a NATS subject for a domain event.
// Format: sentinel.events.{event_type}.{agent_id}
// Example: sentinel.events.agent_action_received.AGENT-07
func BuildEventSubject(eventType, agentID string) string {
	return fmt.Sprintf("%s.%s.%s", SubjectEventsPrefix, eventType, agentID)
}

// BuildAlertSubject creates a NATS subject for a judge alert.
// Format: sentinel.judge.alert.{agent_id}
func BuildAlertSubject(agentID string) string {
	return fmt.Sprintf("%s.alert.%s", SubjectJudgePrefix, agentID)
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
