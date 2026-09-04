package inferencecontract

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

const goldenVectorPath = "../../../schemas/inference/v1/golden-vectors.json"
const controlVectorPath = "../../../schemas/inference/v1/control-vectors.json"

type goldenVector struct {
	RecordType       RecordType `json:"record_type"`
	CanonicalCBORHex string     `json:"canonical_cbor_hex"`
	SHA256           string     `json:"sha256"`
}

type controlVector struct {
	Kind             string `json:"kind"`
	Case             string `json:"case"`
	RecordType       string `json:"record_type,omitempty"`
	Method           string `json:"method,omitempty"`
	Result           string `json:"result,omitempty"`
	CanonicalCBORHex string `json:"canonical_cbor_hex"`
	SHA256           string `json:"sha256"`
}

func fixtureFields(recordType RecordType) map[string]Value {
	fields := make(map[string]Value)
	for index, spec := range schemas[recordType] {
		if !spec.required {
			continue
		}
		switch spec.kind {
		case KindUnsigned:
			fields[spec.name] = Unsigned(1_700_000_000_000)
		case KindText:
			fields[spec.name] = Text(spec.name + "-v1")
		case KindBytes:
			fields[spec.name] = Bytes([]byte{0x01, 0x02, 0x03, 0x04})
		case KindDigest:
			var digest [32]byte
			for offset := range digest {
				digest[offset] = byte(index + 1)
			}
			fields[spec.name] = Digest(digest)
		case KindBool:
			fields[spec.name] = Bool(true)
		}
	}

	switch recordType {
	case BudgetReservation:
		fields["scopes"] = Array([]Value{Object(map[string]Value{
			"scope_kind":                  Text("PROJECT"),
			"scope_id":                    Text("project-v1"),
			"scope_generation_u64":        Unsigned(7),
			"window_kind":                 Text("FIXED_RANGE"),
			"window_start_unix_ms":        Unsigned(1_700_000_000_000),
			"window_end_unix_ms_optional": Unsigned(1_700_000_060_000),
		})})
	case BudgetReservationTransition:
		fields["expected_predecessor_state"] = Text("RESERVED")
		fields["from_state"] = Text("RESERVED")
		fields["to_state"] = Text("RECONCILED")
		fields["transition_reason"] = Text("PROVIDER_USAGE_RECONCILED")
	case BudgetExemption:
		fields["exemption_kind"] = Text("NON_BILLABLE_LOCAL_LOOP")
	case InferenceAdmission, AdmissionDisposition, ProviderDispatchReceipt, ProviderAttemptOutcome, UsageOutcome:
		fields["budget_exemption_id_optional"] = Text("exemption-v1")
		fields["budget_exemption_digest_optional"] = Digest([32]byte{0xee})
	}

	switch recordType {
	case AdmissionDisposition:
		fields["expected_predecessor_state"] = Text("FINAL_ADMITTED")
		fields["disposition"] = Text("PRE_DISPATCH_REJECTED")
		fields["disposition_reason"] = Text("AUTHORITY_DENIED")
	case InferenceAuthorityPort:
		fields["method"] = Text("FINALIZE_ADMISSION")
		fields["record_type"] = Text("InferenceAdmissionV1")
	case ProviderDispatchReceipt:
		fields["expected_predecessor_state"] = Text("FINAL_ADMITTED")
	case ProviderAttemptOutcome:
		fields["expected_predecessor_state"] = Text("DISPATCHED")
		fields["terminal_state"] = Text("COMPLETED")
		fields["terminal_reason"] = Text("PROVIDER_SUCCESS")
	case UsageOutcome:
		fields["resolved_cost_microusd_u64"] = Unsigned(0)
		fields["cost_source"] = Text("CATALOG_COMPUTED")
		fields["terminal"] = Bool(true)
	}
	return fields
}

func TestCanonicalFixturesRoundTrip(t *testing.T) {
	generated := make([]goldenVector, 0, len(AllRecordTypes))
	for _, recordType := range AllRecordTypes {
		record, err := NewRecord(recordType, fixtureFields(recordType))
		if err != nil {
			t.Fatalf("%s: create fixture: %v", recordType, err)
		}
		wire, err := record.CanonicalWirePayload()
		if err != nil {
			t.Fatalf("%s: encode fixture: %v", recordType, err)
		}
		decoded, err := DecodeRecord(recordType, wire)
		if err != nil {
			t.Fatalf("%s: decode fixture: %v", recordType, err)
		}
		if decoded.Digest() != record.Digest() {
			t.Fatalf("%s: digest changed during round trip", recordType)
		}
		generated = append(generated, goldenVector{
			RecordType: recordType, CanonicalCBORHex: hex.EncodeToString(wire), SHA256: record.DigestHex(),
		})
	}
	if os.Getenv("SENTINEL_UPDATE_INFERENCE_GOLDENS") == "1" {
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
	if len(expected) != len(AllRecordTypes) {
		t.Fatalf("expected %d vectors, got %d", len(AllRecordTypes), len(expected))
	}
	for index := range generated {
		if expected[index] != generated[index] {
			t.Fatalf("vector %d differs: expected %+v, got %+v", index, expected[index], generated[index])
		}
	}
}

func TestCompleteTransitionAndPortVectors(t *testing.T) {
	var generated []controlVector
	transitions := []struct{ state, reason string }{
		{"PRE_DISPATCH_RELEASED", "QUEUE_FULL_BEFORE_DISPATCH"},
		{"PRE_DISPATCH_RELEASED", "CLIENT_CANCEL_BEFORE_DISPATCH"},
		{"PRE_DISPATCH_RELEASED", "DEADLINE_BEFORE_DISPATCH"},
		{"DEFINITIVE_NON_BILLABLE_RELEASED", "PROVIDER_DEFINITIVE_NON_BILLABLE"},
		{"RECONCILED", "PROVIDER_USAGE_RECONCILED"},
		{"QUARANTINED", "CLIENT_CANCEL_AFTER_DISPATCH"},
		{"QUARANTINED", "DEADLINE_AFTER_DISPATCH"},
		{"QUARANTINED", "TRANSPORT_LOST"},
		{"QUARANTINED", "INVALID_PROVIDER_RESPONSE"},
		{"QUARANTINED", "GATEWAY_LOST_AFTER_DISPATCH_COMMIT"},
		{"QUARANTINED", "EXPIRED_WITH_UNKNOWN_OUTCOME"},
	}
	for _, transition := range transitions {
		fields := fixtureFields(BudgetReservationTransition)
		fields["to_state"] = Text(transition.state)
		fields["transition_reason"] = Text(transition.reason)
		record, err := NewRecord(BudgetReservationTransition, fields)
		if err != nil {
			t.Fatalf("transition %s/%s: %v", transition.state, transition.reason, err)
		}
		wire, err := record.CanonicalWirePayload()
		if err != nil {
			t.Fatal(err)
		}
		generated = append(generated, controlVector{
			Kind: "reservation_transition", Case: transition.state + ":" + transition.reason,
			RecordType: string(BudgetReservationTransition), CanonicalCBORHex: hex.EncodeToString(wire), SHA256: record.DigestHex(),
		})
	}

	portRecords := []RecordType{
		AdmissionIntent,
		InferenceAdmission,
		AdmissionDisposition,
		ProviderDispatchReceipt,
		ProviderAttemptOutcome,
		UsageOutcome,
	}
	for index, method := range AuthorityPortMethodsV1 {
		fields := fixtureFields(InferenceAuthorityPort)
		fields["method"] = Text(method)
		fields["record_type"] = Text(string(portRecords[index]))
		record, err := NewRecord(InferenceAuthorityPort, fields)
		if err != nil {
			t.Fatalf("port request %s: %v", method, err)
		}
		wire, err := record.CanonicalWirePayload()
		if err != nil {
			t.Fatal(err)
		}
		generated = append(generated, controlVector{
			Kind: "port_request", Case: method, RecordType: string(InferenceAuthorityPort), Method: method,
			CanonicalCBORHex: hex.EncodeToString(wire), SHA256: record.DigestHex(),
		})
	}

	committedDigest := [32]byte{0xa5}
	for _, result := range AllAuthorityResultsV1 {
		method := "FINALIZE_ADMISSION"
		operationID := ""
		var digest *[32]byte
		providerAuthorized := false
		if result == ResultCommitted {
			method = "BEGIN_DISPATCH"
			operationID = "operation-v1"
			digest = &committedDigest
			providerAuthorized = true
		} else if result == ResultReplayedReadback {
			method = "BEGIN_DISPATCH"
			operationID = "operation-v1"
			digest = &committedDigest
		}
		response, err := NewAuthorityResponseV1(method, result, operationID, digest, "", providerAuthorized)
		if err != nil {
			t.Fatalf("port response %s: %v", result, err)
		}
		wire, err := response.CanonicalWirePayload()
		if err != nil {
			t.Fatal(err)
		}
		decoded, err := DecodeAuthorityResponseV1(method, wire)
		if err != nil || decoded.Result() != result {
			t.Fatalf("port response %s round trip: result=%v err=%v", result, decoded, err)
		}
		digestHex, err := response.DigestHex()
		if err != nil {
			t.Fatal(err)
		}
		generated = append(generated, controlVector{
			Kind: "port_response", Case: string(result), Method: method, Result: string(result),
			CanonicalCBORHex: hex.EncodeToString(wire), SHA256: digestHex,
		})
	}

	if os.Getenv("SENTINEL_UPDATE_INFERENCE_GOLDENS") == "1" {
		data, err := json.MarshalIndent(generated, "", "  ")
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(controlVectorPath, append(data, '\n'), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	data, err := os.ReadFile(controlVectorPath)
	if err != nil {
		t.Fatal(err)
	}
	var expected []controlVector
	if err := json.Unmarshal(data, &expected); err != nil {
		t.Fatal(err)
	}
	if len(expected) != 27 {
		t.Fatalf("expected 27 complete control vectors, got %d", len(expected))
	}
	for index := range generated {
		if expected[index] != generated[index] {
			t.Fatalf("control vector %d differs: expected %+v, got %+v", index, expected[index], generated[index])
		}
	}
}

func TestAuthorityResponseNeverReauthorizesReplayOrFailure(t *testing.T) {
	digest := [32]byte{0x44}
	if _, err := NewAuthorityResponseV1("BEGIN_DISPATCH", ResultReplayedReadback, "operation-v1", &digest, "", true); err == nil {
		t.Fatal("replayed readback authorized provider I/O")
	}
	if _, err := NewAuthorityResponseV1("FINALIZE_ADMISSION", ResultCommitted, "operation-v1", &digest, "", true); err == nil {
		t.Fatal("non-dispatch commit authorized provider I/O")
	}
	if _, err := NewAuthorityResponseV1("BEGIN_DISPATCH", ResultDenied, "operation-v1", &digest, "", false); err == nil {
		t.Fatal("failure response carried a committed identity")
	}
}

func TestDecoderRejectsNonCanonicalAndAmbiguousValues(t *testing.T) {
	for _, data := range [][]byte{{0xf6}, {0xfa, 0, 0, 0, 0}, {0x18, 0x01}, {0xbf, 0xff}} {
		if _, err := decodeComplete(data); err == nil {
			t.Fatalf("accepted invalid CBOR %x", data)
		}
	}
	fields := fixtureFields(ProviderCapabilities)
	fields["Capability_Digest"] = Digest([32]byte{0x7f})
	if _, err := NewRecord(ProviderCapabilities, fields); err == nil {
		t.Fatal("accepted case-variant unknown field")
	}
}

func TestSchemaDigestCoversAllTypes(t *testing.T) {
	digest, err := SchemaDigest()
	if err != nil {
		t.Fatal(err)
	}
	const expected = "70a633b63d734fe01e6a0d546148850d405aa362f6a87ede246443f9609457db"
	if digest != expected {
		t.Fatalf("unexpected schema digest %q, want %q", digest, expected)
	}
}
