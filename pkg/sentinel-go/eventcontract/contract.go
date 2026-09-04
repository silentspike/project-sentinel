// Package eventcontract implements the cross-language EventEnvelopeV2 wire
// contract. It is a codec and validation boundary; the Rust event store remains
// the sole append and schema-migration authority.
package eventcontract

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"unicode/utf8"

	"golang.org/x/text/unicode/norm"
)

const (
	ProposalVersionV2       = 2
	CausalContextVersionV1  = 1
	maxAuthorityIDBytes     = 128
	maxWireIDBytes          = 192
	maxEventTypeBytes       = 128
	maxProducerIDBytes      = 128
	maxCausalContextBytes   = 8 * 1024
	maxEventPayloadBytes    = 1024 * 1024
	canonicalUUIDStringSize = 36
)

type AuthorityKindV1 string

const (
	AuthorityTenant   AuthorityKindV1 = "tenant"
	AuthorityCompany  AuthorityKindV1 = "company"
	AuthorityProject  AuthorityKindV1 = "project"
	AuthorityWorkflow AuthorityKindV1 = "workflow"
	AuthorityWorkItem AuthorityKindV1 = "work_item"
)

type EventDurability string

const (
	DurabilityAuthoritative      EventDurability = "authoritative"
	DurabilityDurableOperational EventDurability = "durable_operational"
	DurabilityRebuildable        EventDurability = "rebuildable_telemetry"
)

type EventPayloadCodec string

const (
	PayloadJSON              EventPayloadCodec = "json"
	PayloadDeterministicCBOR EventPayloadCodec = "deterministic_cbor"
)

type CausationPolicyV1 string

const (
	CausationRootRequired   CausationPolicyV1 = "root_required"
	CausationDirectRequired CausationPolicyV1 = "direct_required"
)

type AuthorityRefV1 struct {
	Kind                AuthorityKindV1 `json:"kind"`
	ID                  string          `json:"id"`
	AuthorityGeneration uint64          `json:"authority_generation"`
	AuthorityDigest     string          `json:"authority_digest"`
}

type CausalContextV1 struct {
	SchemaVersion     uint16          `json:"schema_version"`
	Tenant            AuthorityRefV1  `json:"tenant"`
	Company           AuthorityRefV1  `json:"company"`
	Project           AuthorityRefV1  `json:"project"`
	Workflow          *AuthorityRefV1 `json:"workflow,omitempty"`
	WorkItem          *AuthorityRefV1 `json:"work_item,omitempty"`
	RequestID         string          `json:"request_id"`
	RequestDigest     string          `json:"request_digest"`
	CorrelationID     string          `json:"correlation_id"`
	CausationEventID  *string         `json:"causation_event_id,omitempty"`
	OperationID       string          `json:"operation_id"`
	Attempt           uint32          `json:"attempt"`
	SourceGeneration  uint64          `json:"source_generation"`
	SourceDigest      string          `json:"source_digest"`
	InvocationID      *string         `json:"invocation_id,omitempty"`
	AgentID           *string         `json:"agent_id,omitempty"`
	Tick              *uint64         `json:"tick,omitempty"`
	ArtifactID        *string         `json:"artifact_id,omitempty"`
	ArtifactDigest    *string         `json:"artifact_digest,omitempty"`
	QARunID           *string         `json:"qa_run_id,omitempty"`
	ReleaseID         *string         `json:"release_id,omitempty"`
	DeliveryID        *string         `json:"delivery_id,omitempty"`
	DiagnosticTraceID *string         `json:"diagnostic_trace_id,omitempty"`
	DiagnosticSpanID  *string         `json:"diagnostic_span_id,omitempty"`
}

// PayloadBytes preserves Rust's Vec<u8> JSON representation as an integer
// array instead of Go's default base64 string.
type PayloadBytes []byte

func (p PayloadBytes) MarshalJSON() ([]byte, error) {
	values := make([]uint16, len(p))
	for i, value := range p {
		values[i] = uint16(value)
	}
	return json.Marshal(values)
}

func (p *PayloadBytes) UnmarshalJSON(data []byte) error {
	var values []uint16
	if err := decodeStrict(data, &values); err != nil {
		return fmt.Errorf("event contract: payload byte array: %w", err)
	}
	decoded := make([]byte, len(values))
	for i, value := range values {
		if value > 255 {
			return fmt.Errorf("event contract: payload byte %d exceeds 255", i)
		}
		decoded[i] = byte(value)
	}
	*p = decoded
	return nil
}

type ExpectedStreamRevision struct {
	Kind     string  `json:"kind"`
	Revision *uint64 `json:"revision,omitempty"`
}

func NoStream() ExpectedStreamRevision {
	return ExpectedStreamRevision{Kind: "no_stream"}
}

func ExactRevision(revision uint64) ExpectedStreamRevision {
	return ExpectedStreamRevision{Kind: "exact", Revision: &revision}
}

type DeliveryIntentV1 struct {
	IntentID      string `json:"intent_id"`
	Topic         string `json:"topic"`
	PayloadDigest string `json:"payload_digest"`
}

type LocalEffectReservationV1 struct {
	EffectID      string `json:"effect_id"`
	EffectKind    string `json:"effect_kind"`
	RequestDigest string `json:"request_digest"`
}

type StateTransferScope struct {
	World         bool
	NanoContainer string
}

func (s StateTransferScope) MarshalJSON() ([]byte, error) {
	switch {
	case s.World && s.NanoContainer == "":
		return []byte(`"World"`), nil
	case !s.World && s.NanoContainer != "":
		return canonicalJSON(struct {
			NanoContainer string `json:"NanoContainer"`
		}{NanoContainer: s.NanoContainer})
	default:
		return nil, errors.New("event contract: invalid state-transfer scope")
	}
}

func (s *StateTransferScope) UnmarshalJSON(data []byte) error {
	if string(data) == `"World"` {
		*s = StateTransferScope{World: true}
		return nil
	}
	var nano struct {
		NanoContainer string `json:"NanoContainer"`
	}
	if err := decodeStrict(data, &nano); err != nil || nano.NanoContainer == "" {
		return errors.New("event contract: invalid state-transfer scope")
	}
	*s = StateTransferScope{NanoContainer: nano.NanoContainer}
	return nil
}

type OwnerTerm struct {
	Scope                 StateTransferScope `json:"scope"`
	OwnerNode             string             `json:"owner_node"`
	Epoch                 uint64             `json:"epoch"`
	CoordinatorGeneration uint64             `json:"coordinator_generation"`
}

type AppendProposalV2 struct {
	ProposalVersion        uint16                     `json:"proposal_version"`
	RequestedEventID       *string                    `json:"requested_event_id,omitempty"`
	EventType              string                     `json:"event_type"`
	SchemaVersion          uint32                     `json:"schema_version"`
	PayloadCodec           EventPayloadCodec          `json:"payload_codec"`
	PayloadDigest          string                     `json:"payload_digest"`
	Payload                PayloadBytes               `json:"payload"`
	CausalContext          CausalContextV1            `json:"causal_context"`
	Producer               string                     `json:"producer"`
	OwnerTerm              *OwnerTerm                 `json:"owner_term,omitempty"`
	Tick                   *uint64                    `json:"tick,omitempty"`
	RequestedDurability    EventDurability            `json:"requested_durability"`
	ExpectedStreamRevision ExpectedStreamRevision     `json:"expected_stream_revision"`
	DeliveryIntents        []DeliveryIntentV1         `json:"delivery_intents"`
	EffectReservations     []LocalEffectReservationV1 `json:"effect_reservations"`
}

type EventEnvelopeV2 struct {
	EventID                string            `json:"event_id"`
	EventTruthGeneration   uint64            `json:"event_truth_generation"`
	StreamNamespace        string            `json:"stream_namespace"`
	StreamRevision         uint64            `json:"stream_revision"`
	GlobalPosition         uint64            `json:"global_position"`
	EventType              string            `json:"event_type"`
	SchemaVersion          uint32            `json:"schema_version"`
	PayloadCodec           EventPayloadCodec `json:"payload_codec"`
	PayloadDigest          string            `json:"payload_digest"`
	Payload                PayloadBytes      `json:"payload"`
	CausalContext          CausalContextV1   `json:"causal_context"`
	Producer               string            `json:"producer"`
	OwnerTerm              *OwnerTerm        `json:"owner_term,omitempty"`
	Tick                   *uint64           `json:"tick,omitempty"`
	AppendedAtMS           int64             `json:"appended_at_ms"`
	Durability             EventDurability   `json:"durability"`
	CanonicalRequestDigest string            `json:"canonical_request_digest"`
	AppendReceiptDigest    string            `json:"append_receipt_digest"`
	SealedEnvelopeDigest   string            `json:"sealed_envelope_digest"`
}

func DecodeProposalV2(data []byte) (*AppendProposalV2, error) {
	var proposal AppendProposalV2
	if err := decodeStrict(data, &proposal); err != nil {
		return nil, err
	}
	return &proposal, nil
}

func (c CausalContextV1) Validate() error {
	if c.SchemaVersion != CausalContextVersionV1 {
		return fmt.Errorf("event contract: unsupported causal context version %d", c.SchemaVersion)
	}
	for _, expected := range []struct {
		value AuthorityRefV1
		kind  AuthorityKindV1
	}{{c.Tenant, AuthorityTenant}, {c.Company, AuthorityCompany}, {c.Project, AuthorityProject}} {
		if err := expected.value.validate(expected.kind); err != nil {
			return err
		}
	}
	if c.Workflow != nil {
		if err := c.Workflow.validate(AuthorityWorkflow); err != nil {
			return err
		}
	}
	if c.WorkItem != nil {
		if c.Workflow == nil {
			return errors.New("event contract: work item requires workflow authority")
		}
		if err := c.WorkItem.validate(AuthorityWorkItem); err != nil {
			return err
		}
	}
	for field, value := range map[string]string{
		"request_id": c.RequestID, "correlation_id": c.CorrelationID, "operation_id": c.OperationID,
	} {
		if err := validateWireID(field, value, maxWireIDBytes); err != nil {
			return err
		}
	}
	for field, value := range map[string]string{
		"request_digest": c.RequestDigest, "source_digest": c.SourceDigest,
	} {
		if err := validateDigest(field, value); err != nil {
			return err
		}
	}
	if c.CausationEventID != nil && !isCanonicalUUID(*c.CausationEventID, 0) {
		return errors.New("event contract: causation_event_id must be a canonical UUID")
	}
	if c.Attempt == 0 || c.SourceGeneration == 0 {
		return errors.New("event contract: attempt and source_generation must be non-zero")
	}
	for field, value := range map[string]*string{
		"invocation_id": c.InvocationID, "agent_id": c.AgentID, "artifact_id": c.ArtifactID,
		"qa_run_id": c.QARunID, "release_id": c.ReleaseID, "delivery_id": c.DeliveryID,
		"diagnostic_trace_id": c.DiagnosticTraceID, "diagnostic_span_id": c.DiagnosticSpanID,
	} {
		if value != nil {
			if err := validateWireID(field, *value, maxWireIDBytes); err != nil {
				return err
			}
		}
	}
	if (c.ArtifactID == nil) != (c.ArtifactDigest == nil) {
		return errors.New("event contract: artifact identity and digest must be present together")
	}
	if c.ArtifactDigest != nil {
		if err := validateDigest("artifact_digest", *c.ArtifactDigest); err != nil {
			return err
		}
	}
	encoded, err := canonicalJSON(c)
	if err != nil {
		return err
	}
	if len(encoded) > maxCausalContextBytes {
		return errors.New("event contract: causal context exceeds size bound")
	}
	return nil
}

func (c CausalContextV1) AuthorityScopeDigest() (string, error) {
	if err := c.Validate(); err != nil {
		return "", err
	}
	scope := struct {
		Tenant   AuthorityRefV1  `json:"tenant"`
		Company  AuthorityRefV1  `json:"company"`
		Project  AuthorityRefV1  `json:"project"`
		Workflow *AuthorityRefV1 `json:"workflow"`
		WorkItem *AuthorityRefV1 `json:"work_item"`
	}{c.Tenant, c.Company, c.Project, c.Workflow, c.WorkItem}
	return digestJSON(scope)
}

func (p AppendProposalV2) Validate(policy CausationPolicyV1) error {
	if p.ProposalVersion != ProposalVersionV2 || p.SchemaVersion == 0 {
		return errors.New("event contract: invalid proposal or schema version")
	}
	if err := validateWireID("event_type", p.EventType, maxEventTypeBytes); err != nil {
		return err
	}
	if err := validateWireID("producer", p.Producer, maxProducerIDBytes); err != nil {
		return err
	}
	if p.PayloadCodec != PayloadJSON && p.PayloadCodec != PayloadDeterministicCBOR {
		return errors.New("event contract: unknown payload codec")
	}
	if p.RequestedDurability != DurabilityAuthoritative && p.RequestedDurability != DurabilityDurableOperational && p.RequestedDurability != DurabilityRebuildable {
		return errors.New("event contract: unknown durability")
	}
	if len(p.Payload) == 0 || len(p.Payload) > maxEventPayloadBytes || SHA256(p.Payload) != p.PayloadDigest {
		return errors.New("event contract: invalid payload or payload digest")
	}
	if err := p.CausalContext.Validate(); err != nil {
		return err
	}
	if !equalOptionalU64(p.Tick, p.CausalContext.Tick) {
		return errors.New("event contract: tick must match causal context tick")
	}
	hasCausation := p.CausalContext.CausationEventID != nil
	if policy == CausationRootRequired && hasCausation {
		return errors.New("event contract: root event has direct causation")
	}
	if policy == CausationDirectRequired && !hasCausation {
		return errors.New("event contract: non-root event lacks direct causation")
	}
	if policy != CausationRootRequired && policy != CausationDirectRequired {
		return errors.New("event contract: unknown causation policy")
	}
	if p.RequestedEventID != nil && !isCanonicalUUID(*p.RequestedEventID, 7) {
		return errors.New("event contract: requested event ID must be UUIDv7")
	}
	if err := p.ExpectedStreamRevision.validate(); err != nil {
		return err
	}
	return validateIntentSets(p.DeliveryIntents, p.EffectReservations)
}

func (p AppendProposalV2) CanonicalRequestDigest(policy CausationPolicyV1) (string, error) {
	if err := p.Validate(policy); err != nil {
		return "", err
	}
	return digestJSON(p)
}

func (e EventEnvelopeV2) ExpectedAppendReceiptDigest() (string, error) {
	receipt := struct {
		EventID                string          `json:"event_id"`
		EventTruthGeneration   uint64          `json:"event_truth_generation"`
		StreamNamespace        string          `json:"stream_namespace"`
		StreamRevision         uint64          `json:"stream_revision"`
		GlobalPosition         uint64          `json:"global_position"`
		CanonicalRequestDigest string          `json:"canonical_request_digest"`
		AppendedAtMS           int64           `json:"appended_at_ms"`
		Durability             EventDurability `json:"durability"`
	}{e.EventID, e.EventTruthGeneration, e.StreamNamespace, e.StreamRevision, e.GlobalPosition,
		e.CanonicalRequestDigest, e.AppendedAtMS, e.Durability}
	return digestJSON(receipt)
}

func (e EventEnvelopeV2) ExpectedSealedEnvelopeDigest() (string, error) {
	e.SealedEnvelopeDigest = ""
	return digestJSON(e)
}

func (e EventEnvelopeV2) CanonicalEnvelopeDigest() (string, error) {
	return digestJSON(e)
}

func (e EventEnvelopeV2) ValidateSeals() error {
	if !isCanonicalUUID(e.EventID, 7) || e.EventTruthGeneration == 0 || e.StreamRevision == 0 || e.GlobalPosition == 0 || e.AppendedAtMS < 0 {
		return errors.New("event contract: invalid store-owned envelope field")
	}
	if e.SchemaVersion == 0 {
		return errors.New("event contract: invalid envelope schema version")
	}
	if err := validateWireID("event_type", e.EventType, maxEventTypeBytes); err != nil {
		return err
	}
	if err := validateWireID("producer", e.Producer, maxProducerIDBytes); err != nil {
		return err
	}
	if e.PayloadCodec != PayloadJSON && e.PayloadCodec != PayloadDeterministicCBOR {
		return errors.New("event contract: unknown envelope payload codec")
	}
	if e.Durability != DurabilityAuthoritative && e.Durability != DurabilityDurableOperational && e.Durability != DurabilityRebuildable {
		return errors.New("event contract: unknown envelope durability")
	}
	if len(e.Payload) == 0 || len(e.Payload) > maxEventPayloadBytes {
		return errors.New("event contract: invalid envelope payload size")
	}
	for field, value := range map[string]string{
		"payload_digest": e.PayloadDigest, "canonical_request_digest": e.CanonicalRequestDigest,
		"append_receipt_digest": e.AppendReceiptDigest, "sealed_envelope_digest": e.SealedEnvelopeDigest,
	} {
		if err := validateDigest(field, value); err != nil {
			return err
		}
	}
	scope, err := e.CausalContext.AuthorityScopeDigest()
	if err != nil {
		return err
	}
	if scope != e.StreamNamespace || SHA256(e.Payload) != e.PayloadDigest || !equalOptionalU64(e.Tick, e.CausalContext.Tick) {
		return errors.New("event contract: envelope authority, payload, or tick binding mismatch")
	}
	receipt, err := e.ExpectedAppendReceiptDigest()
	if err != nil || receipt != e.AppendReceiptDigest {
		return errors.New("event contract: append receipt digest mismatch")
	}
	sealed, err := e.ExpectedSealedEnvelopeDigest()
	if err != nil || sealed != e.SealedEnvelopeDigest {
		return errors.New("event contract: sealed envelope digest mismatch")
	}
	return nil
}

func CanonicalJSON(value any) ([]byte, error) {
	return canonicalJSON(value)
}

func SHA256(value []byte) string {
	digest := sha256.Sum256(value)
	return hex.EncodeToString(digest[:])
}

func (a AuthorityRefV1) validate(expected AuthorityKindV1) error {
	if a.Kind != expected || a.AuthorityGeneration == 0 {
		return errors.New("event contract: invalid authority hierarchy or generation")
	}
	if err := validateWireID("authority.id", a.ID, maxAuthorityIDBytes); err != nil {
		return err
	}
	return validateDigest("authority_digest", a.AuthorityDigest)
}

func (e ExpectedStreamRevision) validate() error {
	if e.Kind == "no_stream" && e.Revision == nil {
		return nil
	}
	if e.Kind == "exact" && e.Revision != nil {
		return nil
	}
	return errors.New("event contract: invalid expected stream revision")
}

func validateIntentSets(delivery []DeliveryIntentV1, effects []LocalEffectReservationV1) error {
	seen := make(map[string]struct{}, len(delivery)+len(effects))
	for _, item := range delivery {
		if err := validateWireID("delivery_intent.id", item.IntentID, maxWireIDBytes); err != nil {
			return err
		}
		if err := validateWireID("delivery_intent.topic", item.Topic, maxWireIDBytes); err != nil {
			return err
		}
		if err := validateDigest("delivery_intent.payload_digest", item.PayloadDigest); err != nil {
			return err
		}
		if _, exists := seen["delivery:"+item.IntentID]; exists {
			return errors.New("event contract: duplicate delivery intent")
		}
		seen["delivery:"+item.IntentID] = struct{}{}
	}
	for _, item := range effects {
		if err := validateWireID("effect.id", item.EffectID, maxWireIDBytes); err != nil {
			return err
		}
		if err := validateWireID("effect.kind", item.EffectKind, maxWireIDBytes); err != nil {
			return err
		}
		if err := validateDigest("effect.request_digest", item.RequestDigest); err != nil {
			return err
		}
		if _, exists := seen["effect:"+item.EffectID]; exists {
			return errors.New("event contract: duplicate effect reservation")
		}
		seen["effect:"+item.EffectID] = struct{}{}
	}
	return nil
}

func validateWireID(field, value string, maximum int) error {
	if value == "" || len(value) > maximum || !norm.NFC.IsNormalString(value) {
		return fmt.Errorf("event contract: invalid %s", field)
	}
	for _, value := range []byte(value) {
		if !(value >= 'a' && value <= 'z' || value >= 'A' && value <= 'Z' || value >= '0' && value <= '9' || strings.ContainsRune("-_.:/", rune(value))) {
			return fmt.Errorf("event contract: invalid %s", field)
		}
	}
	return nil
}

func validateDigest(field, value string) error {
	if len(value) != sha256.Size*2 {
		return fmt.Errorf("event contract: invalid %s", field)
	}
	for _, value := range []byte(value) {
		if !(value >= '0' && value <= '9' || value >= 'a' && value <= 'f') {
			return fmt.Errorf("event contract: invalid %s", field)
		}
	}
	return nil
}

func isCanonicalUUID(value string, version byte) bool {
	if len(value) != canonicalUUIDStringSize || value[8] != '-' || value[13] != '-' || value[18] != '-' || value[23] != '-' {
		return false
	}
	for i, character := range []byte(value) {
		if i == 8 || i == 13 || i == 18 || i == 23 {
			continue
		}
		if !(character >= '0' && character <= '9' || character >= 'a' && character <= 'f') {
			return false
		}
	}
	if version != 0 && value[14] != version+'0' {
		return false
	}
	return strings.ContainsRune("89ab", rune(value[19]))
}

func equalOptionalU64(left, right *uint64) bool {
	return left == nil && right == nil || left != nil && right != nil && *left == *right
}

func canonicalJSON(value any) ([]byte, error) {
	var buffer bytes.Buffer
	encoder := json.NewEncoder(&buffer)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(value); err != nil {
		return nil, fmt.Errorf("event contract: canonical JSON: %w", err)
	}
	encoded := bytes.TrimSuffix(buffer.Bytes(), []byte{'\n'})
	if !utf8.Valid(encoded) {
		return nil, errors.New("event contract: canonical JSON is not UTF-8")
	}
	return encoded, nil
}

func digestJSON(value any) (string, error) {
	encoded, err := canonicalJSON(value)
	if err != nil {
		return "", err
	}
	return SHA256(encoded), nil
}

func decodeStrict(data []byte, target any) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(target); err != nil {
		return err
	}
	if decoder.More() {
		return errors.New("event contract: trailing JSON value")
	}
	return nil
}
