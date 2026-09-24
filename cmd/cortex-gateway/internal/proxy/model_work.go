package proxy

import (
	"encoding/hex"
	"errors"
	"regexp"
	"strconv"
	"strings"
	"time"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/detection"
)

var adaptiveRequestID = regexp.MustCompile(`^company-adaptive-([0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})-([0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})$`)

const (
	maxModelWorkResponseBytes = 128 * 1024
	maxModelWorkDuration      = 120 * time.Second
)

// This marker selects a response contract, not execution authority. The daemon
// derives and revalidates the actual workflow authority before admitting tools.
func classifyModelWorkRequest(req *LLMRequest, requestID string) (bool, error) {
	schema, present := req.Metadata["company_execution_schema"]
	if !present {
		if hasCustomerRequestMetadata(req.Metadata) {
			return false, errors.New("customer request execution schema is missing")
		}
		return false, nil
	}
	invalid := errors.New("invalid company execution request")
	if req.RequestClass != RequestClassAgentRuntime || req.Stream || req.MaxTokens <= 0 {
		return false, invalid
	}
	if !metadataValuesPresent(req.Metadata, "tenant_id", "reservation_id", "reserved_provider") {
		return false, invalid
	}
	if !validCompanyExecutionSubject(req.Metadata, requestID, schema) {
		return false, invalid
	}
	if req.Metadata["request_id"] != requestID || !companyExecutionRequestIdentity(requestID, req.Metadata, schema) {
		return false, invalid
	}
	digest := req.Metadata["company_execution_context_digest"]
	decoded, err := hex.DecodeString(digest)
	if err != nil || len(decoded) != 32 || digest != strings.ToLower(digest) {
		return false, invalid
	}
	return true, nil
}

func companyExecutionRequestIdentity(requestID string, metadata map[string]string, schema string) bool {
	switch schema {
	case "3":
		return adaptiveRequestIdentity(requestID, metadata)
	case "4":
		return requestID == "company-planning-"+metadata["reservation_id"]+"-"+metadata["project_id"]
	default:
		return requestID == "company-provider-"+metadata["reservation_id"]
	}
}

func metadataValuesPresent(metadata map[string]string, keys ...string) bool {
	for _, key := range keys {
		if strings.TrimSpace(metadata[key]) == "" {
			return false
		}
	}
	return true
}

func validCompanyExecutionSubject(metadata map[string]string, requestID, schema string) bool {
	projectBinding := []string{"project_id", "work_item_id", "assignment_id", "assignment_version"}
	switch schema {
	case "1":
		return metadataValuesPresent(metadata, projectBinding...) && !hasCustomerRequestMetadata(metadata)
	case "2":
		_, err := customerRequestSubject(metadata)
		return err == nil
	case "3":
		adaptiveBinding := append(projectBinding, "adaptive_session_id", "adaptive_effect_id", "adaptive_session_version")
		return metadataValuesPresent(metadata, adaptiveBinding...) &&
			!hasCustomerRequestMetadata(metadata) && adaptiveRequestIdentity(requestID, metadata)
	case "4":
		_, err := projectPlanningSubject(metadata)
		return err == nil
	default:
		return false
	}
}

func adaptiveRequestIdentity(requestID string, metadata map[string]string) bool {
	parts := adaptiveRequestID.FindStringSubmatch(requestID)
	return len(parts) == 3 && parts[1] == metadata["adaptive_session_id"] && parts[2] == metadata["adaptive_effect_id"]
}

// Pre-agreement Sales work belongs to a customer request, never a synthetic
// project. The daemon authenticates the principal and validates this version
// against durable state; metadata alone grants no permission to call a provider.
type customerRequestExecutionSubject struct {
	Kind           string `json:"kind"`
	RequestID      string `json:"request_id,omitempty"`
	RequestVersion uint64 `json:"request_version,omitempty"`
	SessionID      string `json:"session_id,omitempty"`
	EffectID       string `json:"effect_id,omitempty"`
	SessionVersion uint64 `json:"session_version,omitempty"`
	ProjectID      string `json:"project_id,omitempty"`
	ProjectVersion uint64 `json:"project_version,omitempty"`
}

func projectPlanningSubject(metadata map[string]string) (*customerRequestExecutionSubject, error) {
	invalid := errors.New("invalid project planning execution subject")
	if metadata["company_execution_schema"] != "4" || metadata["company_execution_subject"] != "project_planning" || !subscriptionIdentifier.MatchString(metadata["project_id"]) {
		return nil, invalid
	}
	for _, key := range []string{"customer_request_id", "customer_request_version", "work_item_id", "assignment_id", "assignment_version"} {
		if _, present := metadata[key]; present {
			return nil, invalid
		}
	}
	versionText := metadata["project_version"]
	version, err := strconv.ParseUint(versionText, 10, 64)
	if err != nil || version == 0 || strconv.FormatUint(version, 10) != versionText {
		return nil, invalid
	}
	return &customerRequestExecutionSubject{Kind: "project_planning", ProjectID: metadata["project_id"], ProjectVersion: version}, nil
}

func hasCustomerRequestMetadata(metadata map[string]string) bool {
	for _, key := range []string{"company_execution_subject", "customer_request_id", "customer_request_version"} {
		if _, present := metadata[key]; present {
			return true
		}
	}
	return false
}

func customerRequestSubject(metadata map[string]string) (*customerRequestExecutionSubject, error) {
	invalid := errors.New("invalid customer request execution subject")
	if metadata["company_execution_schema"] != "2" || metadata["company_execution_subject"] != "customer_request" || !subscriptionIdentifier.MatchString(metadata["customer_request_id"]) {
		return nil, invalid
	}
	for _, key := range []string{"project_id", "work_item_id", "assignment_id", "assignment_version"} {
		if _, present := metadata[key]; present {
			return nil, invalid
		}
	}
	versionText := metadata["customer_request_version"]
	version, err := strconv.ParseUint(versionText, 10, 64)
	if err != nil || version == 0 || strconv.FormatUint(version, 10) != versionText {
		return nil, invalid
	}
	return &customerRequestExecutionSubject{Kind: "customer_request", RequestID: metadata["customer_request_id"], RequestVersion: version}, nil
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
