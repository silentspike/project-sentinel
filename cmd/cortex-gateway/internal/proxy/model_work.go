package proxy

import (
	"encoding/hex"
	"errors"
	"strings"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/detection"
)

const maxModelWorkResponseBytes = 128 * 1024

// This marker selects a response contract, not execution authority. The daemon
// derives and revalidates the actual workflow authority before admitting tools.
func classifyModelWorkRequest(req *LLMRequest, requestID string) (bool, error) {
	schema, present := req.Metadata["company_execution_schema"]
	if !present {
		return false, nil
	}
	invalid := errors.New("invalid company execution request")
	if schema != "1" || req.RequestClass != RequestClassAgentRuntime || req.Stream || req.MaxTokens <= 0 {
		return false, invalid
	}
	for _, key := range []string{"tenant_id", "project_id", "work_item_id", "reservation_id", "assignment_id", "assignment_version", "reserved_provider"} {
		if strings.TrimSpace(req.Metadata[key]) == "" {
			return false, invalid
		}
	}
	if requestID != "company-provider-"+req.Metadata["reservation_id"] || req.Metadata["request_id"] != requestID {
		return false, invalid
	}
	digest := req.Metadata["company_execution_context_digest"]
	decoded, err := hex.DecodeString(digest)
	if err != nil || len(decoded) != 32 || digest != strings.ToLower(digest) {
		return false, invalid
	}
	return true, nil
}

// Typed work proposals cannot be regenerated under one provider reservation.
// Run the existing deterministic checks, conservatively reject any concern,
// and leave recovery/rework to an explicitly authorized new workflow operation.
func (ph *PipelineHandler) modelWorkResponseAllowed(content, agentName string, snap control.ConfigSnapshot) bool {
	if len(content) == 0 || len(content) > maxModelWorkResponseBytes {
		return false
	}
	if detected, _ := detection.DetectFourthWall(content); detected {
		return false
	}
	if ph.drift != nil && snap.PersonalityGuardEnabled {
		result := ph.drift.CheckDrift(agentName, []string{content})
		personalityGuardDriftTotal.WithLabelValues(agentName, result.Severity).Inc()
		if result.DriftScore >= snap.DriftThreshold {
			return false
		}
	}
	if ph.quality != nil && snap.QualityGateEnabled {
		result := ph.quality.ScoreMessage(agentName, content, nil)
		qualityGateScore.WithLabelValues(agentName).Observe(float64(result.Score))
		if result.Score <= snap.QualityThreshold {
			return false
		}
	}
	return true
}
