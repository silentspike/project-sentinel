package eventcontract

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

const goldenVectorPath = "../../../schemas/event/v2/golden-vectors.json"

type goldenVector struct {
	Case                       string            `json:"case"`
	CausationPolicy            CausationPolicyV1 `json:"causation_policy"`
	CausalContextCanonicalJSON string            `json:"causal_context_canonical_json"`
	CausalContextSHA256        string            `json:"causal_context_sha256"`
	AuthorityScopeSHA256       string            `json:"authority_scope_sha256"`
	Proposal                   AppendProposalV2  `json:"proposal"`
	ProposalCanonicalJSON      string            `json:"proposal_canonical_json"`
	ProposalSHA256             string            `json:"proposal_sha256"`
	Envelope                   EventEnvelopeV2   `json:"envelope"`
	EnvelopeCanonicalJSON      string            `json:"envelope_canonical_json"`
	EnvelopeSHA256             string            `json:"envelope_sha256"`
}

func TestCustomerAdmissionThroughDeliveryGoldenVectors(t *testing.T) {
	generated := journeyVectors(t)
	if os.Getenv("SENTINEL_UPDATE_EVENT_GOLDENS") == "1" {
		data, err := json.MarshalIndent(generated, "", "  ")
		if err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll(filepath.Dir(goldenVectorPath), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(goldenVectorPath, append(data, '\n'), 0o644); err != nil {
			t.Fatal(err)
		}
	}

	data, err := os.ReadFile(goldenVectorPath)
	if err != nil {
		t.Fatal(err)
	}
	var expected []goldenVector
	if err := json.Unmarshal(data, &expected); err != nil {
		t.Fatal(err)
	}
	if len(expected) != len(generated) {
		t.Fatalf("expected %d vectors, got %d", len(generated), len(expected))
	}
	for index := range generated {
		assertVectorEqual(t, expected[index], generated[index])
	}
}

func TestCausationAndPayloadMutationsFailClosed(t *testing.T) {
	vectors := journeyVectors(t)
	root := vectors[0].Proposal
	root.CausalContext.CausationEventID = stringPointer(vectors[0].Envelope.EventID)
	if err := root.Validate(CausationRootRequired); err == nil {
		t.Fatal("root event accepted direct causation")
	}

	direct := vectors[1].Proposal
	direct.CausalContext.CausationEventID = nil
	if err := direct.Validate(CausationDirectRequired); err == nil {
		t.Fatal("non-root event accepted missing causation")
	}

	tampered := vectors[2].Proposal
	tampered.Payload = append(PayloadBytes(nil), tampered.Payload...)
	tampered.Payload[0] ^= 1
	if err := tampered.Validate(CausationDirectRequired); err == nil {
		t.Fatal("proposal accepted a tampered payload")
	}

	encoded, err := CanonicalJSON(vectors[0].Proposal)
	if err != nil {
		t.Fatal(err)
	}
	encoded = append(encoded[:len(encoded)-1], []byte(`,"unknown":true}`)...)
	if _, err := DecodeProposalV2(encoded); err == nil {
		t.Fatal("proposal accepted an unknown field")
	}

	envelope := vectors[1].Envelope
	wrongTick := *envelope.Tick + 1
	envelope.Tick = &wrongTick
	envelope.SealedEnvelopeDigest, err = envelope.ExpectedSealedEnvelopeDigest()
	if err != nil {
		t.Fatal(err)
	}
	if err := envelope.ValidateSeals(); err == nil {
		t.Fatal("envelope accepted a tick outside its causal context")
	}
}

func journeyVectors(t *testing.T) []goldenVector {
	t.Helper()
	cases := []string{
		"customer_admission",
		"project_creation",
		"work_claim",
		"artifact_commit",
		"qa_approval",
		"delivery_acceptance",
	}
	eventIDs := []string{
		"01890f3d-0000-7000-8000-000000000001",
		"01890f3d-0000-7000-8000-000000000002",
		"01890f3d-0000-7000-8000-000000000003",
		"01890f3d-0000-7000-8000-000000000004",
		"01890f3d-0000-7000-8000-000000000005",
		"01890f3d-0000-7000-8000-000000000006",
	}
	base := CausalContextV1{
		SchemaVersion:     CausalContextVersionV1,
		Tenant:            authority(AuthorityTenant, "tenant-demo"),
		Company:           authority(AuthorityCompany, "company-demo"),
		Project:           authority(AuthorityProject, "project-web"),
		Workflow:          authorityPointer(AuthorityWorkflow, "workflow-web"),
		RequestID:         "request-web-001",
		RequestDigest:     SHA256([]byte("request-web-001")),
		CorrelationID:     "correlation-web-001",
		Attempt:           1,
		SourceGeneration:  1,
		SourceDigest:      SHA256([]byte("workflow-source-v1")),
		DiagnosticTraceID: stringPointer("trace-web-001"),
		DiagnosticSpanID:  stringPointer("span-admission"),
	}
	vectors := make([]goldenVector, 0, len(cases))
	for index, name := range cases {
		context := base
		context.OperationID = "operation-" + name
		tick := uint64(100 + index)
		context.Tick = &tick
		context.DiagnosticSpanID = stringPointer("span-" + name)
		policy := CausationDirectRequired
		expectedRevision := ExactRevision(uint64(index))
		if index == 0 {
			policy = CausationRootRequired
			expectedRevision = NoStream()
		} else {
			context.CausationEventID = stringPointer(eventIDs[index-1])
		}
		if index >= 2 {
			context.AgentID = stringPointer("AGENT-05")
		}
		if index >= 3 {
			context.ArtifactID = stringPointer("artifact-web-001")
			context.ArtifactDigest = stringPointer(SHA256([]byte("artifact-web-001")))
		}
		if index >= 4 {
			context.QARunID = stringPointer("qa-web-001")
		}
		if index >= 5 {
			context.ReleaseID = stringPointer("release-web-001")
			context.DeliveryID = stringPointer("delivery-web-001")
		}
		payload := PayloadBytes([]byte(`{"stage":"` + name + `"}`))
		proposal := AppendProposalV2{
			ProposalVersion: ProposalVersionV2, RequestedEventID: stringPointer(eventIDs[index]),
			EventType: name, SchemaVersion: 1, PayloadCodec: PayloadJSON,
			PayloadDigest: SHA256(payload), Payload: payload, CausalContext: context,
			Producer: "workflow", Tick: &tick, RequestedDurability: DurabilityAuthoritative,
			ExpectedStreamRevision: expectedRevision, DeliveryIntents: []DeliveryIntentV1{},
			EffectReservations: []LocalEffectReservationV1{},
		}
		if name == "work_claim" {
			proposal.EffectReservations = []LocalEffectReservationV1{{
				EffectID: "effect-workbench-001", EffectKind: "workbench",
				RequestDigest: SHA256([]byte("effect-workbench-001")),
			}}
		}
		if name == "delivery_acceptance" {
			proposal.DeliveryIntents = []DeliveryIntentV1{{
				IntentID: "delivery-intent-001", Topic: "sentinel/workflow/delivery",
				PayloadDigest: SHA256([]byte("delivery-intent-001")),
			}}
		}
		proposalJSON, requestDigest := mustCanonicalAndDigest(t, proposal)
		contextJSON, contextDigest := mustCanonicalAndDigest(t, context)
		scopeDigest, err := context.AuthorityScopeDigest()
		if err != nil {
			t.Fatal(err)
		}
		if actual, err := proposal.CanonicalRequestDigest(policy); err != nil || actual != requestDigest {
			t.Fatalf("%s proposal validation: %v, digest %s", name, err, actual)
		}
		envelope := EventEnvelopeV2{
			EventID: eventIDs[index], EventTruthGeneration: 1, StreamNamespace: scopeDigest,
			StreamRevision: uint64(index + 1), GlobalPosition: uint64(index + 1),
			EventType: proposal.EventType, SchemaVersion: proposal.SchemaVersion,
			PayloadCodec: proposal.PayloadCodec, PayloadDigest: proposal.PayloadDigest,
			Payload: proposal.Payload, CausalContext: context, Producer: proposal.Producer,
			Tick: &tick, AppendedAtMS: 1_700_000_000_000 + int64(index),
			Durability: proposal.RequestedDurability, CanonicalRequestDigest: requestDigest,
		}
		envelope.AppendReceiptDigest, err = envelope.ExpectedAppendReceiptDigest()
		if err != nil {
			t.Fatal(err)
		}
		envelope.SealedEnvelopeDigest, err = envelope.ExpectedSealedEnvelopeDigest()
		if err != nil {
			t.Fatal(err)
		}
		if err := envelope.ValidateSeals(); err != nil {
			t.Fatalf("%s envelope: %v", name, err)
		}
		envelopeJSON, envelopeDigest := mustCanonicalAndDigest(t, envelope)
		vectors = append(vectors, goldenVector{
			Case: name, CausationPolicy: policy,
			CausalContextCanonicalJSON: string(contextJSON), CausalContextSHA256: contextDigest,
			AuthorityScopeSHA256: scopeDigest, Proposal: proposal,
			ProposalCanonicalJSON: string(proposalJSON), ProposalSHA256: requestDigest,
			Envelope: envelope, EnvelopeCanonicalJSON: string(envelopeJSON), EnvelopeSHA256: envelopeDigest,
		})
	}
	return vectors
}

func authority(kind AuthorityKindV1, id string) AuthorityRefV1 {
	return AuthorityRefV1{Kind: kind, ID: id, AuthorityGeneration: 1, AuthorityDigest: SHA256([]byte(id))}
}

func authorityPointer(kind AuthorityKindV1, id string) *AuthorityRefV1 {
	value := authority(kind, id)
	return &value
}

func stringPointer(value string) *string { return &value }

func mustCanonicalAndDigest(t *testing.T, value any) ([]byte, string) {
	t.Helper()
	encoded, err := CanonicalJSON(value)
	if err != nil {
		t.Fatal(err)
	}
	return encoded, SHA256(encoded)
}

func assertVectorEqual(t *testing.T, expected, actual goldenVector) {
	t.Helper()
	expectedJSON, err := json.Marshal(expected)
	if err != nil {
		t.Fatal(err)
	}
	actualJSON, err := json.Marshal(actual)
	if err != nil {
		t.Fatal(err)
	}
	if string(expectedJSON) != string(actualJSON) {
		t.Fatalf("vector %s differs\nexpected: %s\nactual:   %s", actual.Case, expectedJSON, actualJSON)
	}
}
