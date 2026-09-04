package inferencecontract

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"sort"
)

type fieldSpec struct {
	name     string
	kind     ValueKind
	required bool
}

func required(name string, kind ValueKind) fieldSpec {
	return fieldSpec{name: name, kind: kind, required: true}
}

func optional(name string, kind ValueKind) fieldSpec {
	return fieldSpec{name: name, kind: kind}
}

var schemas = map[RecordType][]fieldSpec{
	AdmissionIntent: {
		required("admission_intent_id", KindText), required("request_id", KindText),
		required("request_digest", KindDigest), required("request_class", KindText),
		optional("agent_id_optional", KindText), required("caller_service_identity", KindText),
		required("authenticated_principal_digest", KindDigest), required("tenant_id", KindText),
		required("project_id", KindText), required("work_item_id", KindText),
		required("agreement_id", KindText), required("customer_id", KindText),
		required("governance_receipt_id", KindText), required("governance_receipt_digest", KindDigest),
		required("governance_generation_u64", KindUnsigned), required("provider_id_proposal", KindText),
		required("model_id_proposal", KindText), required("catalog_digest_proposal", KindDigest),
		required("capability_digest_proposal", KindDigest), required("pricing_digest_proposal", KindDigest),
		optional("hierarchy_tier_optional", KindText), optional("requested_max_input_tokens_optional_u64", KindUnsigned),
		required("requested_max_output_tokens_u64", KindUnsigned),
		required("provider_execution_deadline_proposal_unix_ms", KindUnsigned),
		required("queue_policy_id", KindText),
	},
	BudgetReservation: {
		required("reservation_id", KindText), required("admission_intent_id", KindText),
		required("admission_intent_digest", KindDigest), required("request_id", KindText),
		required("request_digest", KindDigest), required("governance_receipt_id", KindText),
		required("governance_receipt_digest", KindDigest), required("governance_generation_u64", KindUnsigned),
		required("provider_id", KindText), required("model_id", KindText),
		required("hierarchy_policy_digest", KindDigest), required("routing_policy_generation_u64", KindUnsigned),
		required("catalog_digest", KindDigest), required("capability_digest", KindDigest),
		required("pricing_digest", KindDigest), required("effective_max_input_tokens_u64", KindUnsigned),
		required("effective_max_output_tokens_u64", KindUnsigned),
		required("effective_provider_execution_deadline_unix_ms", KindUnsigned),
		required("scopes", KindArray), required("reserved_microusd_u64", KindUnsigned),
		required("estimated_input_microusd_u64", KindUnsigned), required("expires_at_unix_ms", KindUnsigned),
	},
	BudgetReservationTransition: {
		required("transition_operation_id", KindText), required("reservation_id", KindText),
		required("reservation_digest", KindDigest), required("expected_predecessor_operation_id", KindText),
		required("expected_predecessor_state", KindText), required("from_state", KindText),
		required("to_state", KindText), required("transition_reason", KindText),
		optional("authority_evidence_digest_optional", KindDigest), required("occurred_at_unix_ms", KindUnsigned),
	},
	BudgetExemption: {
		required("exemption_id", KindText), required("admission_intent_id", KindText),
		required("admission_intent_digest", KindDigest), required("governance_receipt_id", KindText),
		required("governance_receipt_digest", KindDigest), required("governance_generation_u64", KindUnsigned),
		required("exemption_kind", KindText), required("authorized_service_identity", KindText),
		required("authorized_reason_digest", KindDigest), required("expires_at_unix_ms", KindUnsigned),
	},
	InferenceAdmission: {
		required("admission_id", KindText), required("admission_intent_id", KindText),
		required("admission_intent_digest", KindDigest), required("request_id", KindText),
		required("request_digest", KindDigest), required("provider_id", KindText), required("model_id", KindText),
		required("hierarchy_policy_digest", KindDigest), required("routing_policy_generation_u64", KindUnsigned),
		required("catalog_digest", KindDigest), required("capability_digest", KindDigest),
		required("pricing_digest", KindDigest), required("effective_max_input_tokens_u64", KindUnsigned),
		required("effective_max_output_tokens_u64", KindUnsigned),
		required("provider_execution_deadline_unix_ms", KindUnsigned), required("queue_policy_id", KindText),
		optional("budget_reservation_id_optional", KindText), optional("budget_reservation_digest_optional", KindDigest),
		optional("budget_exemption_id_optional", KindText), optional("budget_exemption_digest_optional", KindDigest),
		required("finalized_at_unix_ms", KindUnsigned),
	},
	AdmissionDisposition: {
		required("disposition_operation_id", KindText), required("admission_id", KindText),
		required("admission_digest", KindDigest), required("expected_predecessor_state", KindText),
		required("disposition", KindText), required("disposition_reason", KindText),
		optional("diagnostic_digest_optional", KindDigest),
		optional("budget_reservation_id_optional", KindText), optional("budget_reservation_digest_optional", KindDigest),
		optional("budget_exemption_id_optional", KindText), optional("budget_exemption_digest_optional", KindDigest),
		required("occurred_at_unix_ms", KindUnsigned),
	},
	InferenceAuthorityPort: {
		required("method", KindText), required("caller_service_identity", KindText),
		required("authenticated_principal_digest", KindDigest), required("idempotency_key", KindText),
		required("record_type", KindText), required("record_id", KindText),
		required("record_payload_digest", KindDigest), optional("expected_predecessor_operation_id_optional", KindText),
		optional("expected_predecessor_state_optional", KindText), required("typed_payload", KindBytes),
	},
	ProviderDispatchReceipt: {
		required("dispatch_operation_id", KindText), required("admission_id", KindText),
		required("admission_digest", KindDigest), required("attempt_id", KindText),
		required("attempt_binding_digest", KindDigest), required("expected_predecessor_state", KindText),
		required("provider_id", KindText), required("model_id", KindText),
		required("catalog_digest", KindDigest), required("capability_digest", KindDigest),
		optional("budget_reservation_id_optional", KindText), optional("budget_reservation_digest_optional", KindDigest),
		optional("budget_exemption_id_optional", KindText), optional("budget_exemption_digest_optional", KindDigest),
		optional("provider_request_id_optional", KindText), required("occurred_at_unix_ms", KindUnsigned),
	},
	ProviderAttemptOutcome: {
		required("outcome_operation_id", KindText), required("admission_id", KindText),
		required("admission_digest", KindDigest), required("request_id", KindText), required("request_digest", KindDigest),
		required("attempt_id", KindText), required("attempt_binding_digest", KindDigest),
		required("dispatch_operation_id", KindText), required("expected_predecessor_state", KindText),
		required("provider_id", KindText), required("model_id", KindText),
		required("catalog_digest", KindDigest), required("capability_digest", KindDigest),
		optional("budget_reservation_id_optional", KindText), optional("budget_reservation_digest_optional", KindDigest),
		optional("budget_exemption_id_optional", KindText), optional("budget_exemption_digest_optional", KindDigest),
		required("terminal_state", KindText), required("terminal_reason", KindText),
		optional("provider_request_id_optional", KindText), optional("authority_evidence_digest_optional", KindDigest),
		required("occurred_at_unix_ms", KindUnsigned),
	},
	UsageOutcome: {
		required("usage_operation_id", KindText), required("attempt_id", KindText),
		required("attempt_binding_digest", KindDigest), required("terminal_outcome_operation_id", KindText),
		required("terminal_outcome_payload_digest", KindDigest),
		optional("budget_reservation_id_optional", KindText), optional("budget_reservation_digest_optional", KindDigest),
		optional("budget_exemption_id_optional", KindText), optional("budget_exemption_digest_optional", KindDigest),
		required("input_tokens_u64", KindUnsigned), required("output_tokens_u64", KindUnsigned),
		required("cache_read_input_tokens_u64", KindUnsigned), required("cache_creation_input_tokens_u64", KindUnsigned),
		optional("reported_cost_microusd_u64_optional", KindUnsigned), required("resolved_cost_microusd_u64", KindUnsigned),
		required("cost_source", KindText), required("terminal", KindBool), required("partial_stream", KindBool),
		required("occurred_at_unix_ms", KindUnsigned),
	},
	ProviderCapabilities: {
		required("provider_id", KindText), required("model_id", KindText),
		required("catalog_digest", KindDigest), required("request_format_digest", KindDigest),
		required("supports_streaming", KindBool), required("supports_usage_in_stream", KindBool),
		required("supports_structured_output", KindBool), required("supports_tool_use", KindBool),
		required("supports_inventory", KindBool), required("supports_cache_accounting", KindBool),
		required("supports_cancellation", KindBool), required("supports_definitive_rejection", KindBool),
		required("supports_status_reporting", KindBool), required("supports_retry_after", KindBool),
	},
}

var digestFields = map[RecordType]string{
	AdmissionIntent: "admission_intent_digest", BudgetReservation: "reservation_digest",
	BudgetReservationTransition: "transition_payload_digest", BudgetExemption: "exemption_digest",
	InferenceAdmission: "admission_digest", AdmissionDisposition: "disposition_payload_digest",
	InferenceAuthorityPort: "authority_request_digest", ProviderDispatchReceipt: "dispatch_payload_digest",
	ProviderAttemptOutcome: "outcome_payload_digest", UsageOutcome: "usage_payload_digest",
	ProviderCapabilities: "capability_digest",
}

func ownDigestField(recordType RecordType) (string, error) {
	field, ok := digestFields[recordType]
	if !ok {
		return "", fmt.Errorf("inference contract: unknown record type %s", recordType)
	}
	return field, nil
}

func validateRelations(recordType RecordType, fields map[string]Value) error {
	switch recordType {
	case BudgetReservation:
		return validateScopes(fields)
	case BudgetReservationTransition:
		if err := requireText(fields, "expected_predecessor_state", "RESERVED"); err != nil {
			return err
		}
		if err := requireText(fields, "from_state", "RESERVED"); err != nil {
			return err
		}
		to, err := textField(fields, "to_state")
		if err != nil {
			return err
		}
		reason, err := textField(fields, "transition_reason")
		if err != nil {
			return err
		}
		if !legalReservationTransition(to, reason) {
			return invalid("transition_reason", "illegal reservation transition")
		}
	case BudgetExemption:
		return requireOneOf(fields, "exemption_kind", "NON_BILLABLE_LOCAL_LOOP", "NON_BILLABLE_FAKE_PROVIDER_TEST")
	case InferenceAdmission:
		return validateAuthorityPair(fields)
	case AdmissionDisposition:
		if err := validateAuthorityPair(fields); err != nil {
			return err
		}
		if err := requireText(fields, "expected_predecessor_state", "FINAL_ADMITTED"); err != nil {
			return err
		}
		disposition, err := textField(fields, "disposition")
		if err != nil {
			return err
		}
		reason, err := textField(fields, "disposition_reason")
		if err != nil {
			return err
		}
		if !legalDisposition(disposition, reason) {
			return invalid("disposition_reason", "illegal disposition state/reason")
		}
	case InferenceAuthorityPort:
		if err := requireOneOf(fields, "method", "RESERVE_OR_EXEMPT", "FINALIZE_ADMISSION", "WIN_PRE_DISPATCH_DISPOSITION", "BEGIN_DISPATCH", "COMMIT_ATTEMPT_OUTCOME", "RECONCILE_USAGE"); err != nil {
			return err
		}
		recordType, err := textField(fields, "record_type")
		if err != nil {
			return err
		}
		known := false
		for _, candidate := range AllRecordTypes {
			if string(candidate) == recordType {
				known = true
				break
			}
		}
		if !known {
			return invalid("record_type", "unexpected closed-enum value")
		}
		_, err = pairPresent(fields, "expected_predecessor_operation_id_optional", "expected_predecessor_state_optional")
		return err
	case ProviderDispatchReceipt:
		if err := validateAuthorityPair(fields); err != nil {
			return err
		}
		return requireText(fields, "expected_predecessor_state", "FINAL_ADMITTED")
	case ProviderAttemptOutcome:
		if err := validateAuthorityPair(fields); err != nil {
			return err
		}
		if err := requireText(fields, "expected_predecessor_state", "DISPATCHED"); err != nil {
			return err
		}
		state, err := textField(fields, "terminal_state")
		if err != nil {
			return err
		}
		reason, err := textField(fields, "terminal_reason")
		if err != nil {
			return err
		}
		if !legalTerminalOutcome(state, reason) {
			return invalid("terminal_reason", "illegal terminal state/reason")
		}
	case UsageOutcome:
		if err := validateAuthorityPair(fields); err != nil {
			return err
		}
		if terminal, ok := fields["terminal"]; !ok || terminal.kind != KindBool || !terminal.flag {
			return invalid("terminal", "usage must be terminal")
		}
		if err := requireOneOf(fields, "cost_source", "PROVIDER_REPORTED", "CATALOG_COMPUTED", "CONSERVATIVE_RESERVED"); err != nil {
			return err
		}
		if _, exempt := fields["budget_exemption_id_optional"]; exempt {
			cost, err := unsignedField(fields, "resolved_cost_microusd_u64")
			if err != nil {
				return err
			}
			if cost != 0 {
				return invalid("resolved_cost_microusd_u64", "exempt usage must cost zero")
			}
		}
	}
	return nil
}

func validateAuthorityPair(fields map[string]Value) error {
	reservation, err := pairPresent(fields, "budget_reservation_id_optional", "budget_reservation_digest_optional")
	if err != nil {
		return err
	}
	exemption, err := pairPresent(fields, "budget_exemption_id_optional", "budget_exemption_digest_optional")
	if err != nil {
		return err
	}
	if reservation == exemption {
		return invalid("budget_authority", "exactly one reservation or exemption pair is required")
	}
	return nil
}

func pairPresent(fields map[string]Value, id, digest string) (bool, error) {
	_, hasID := fields[id]
	_, hasDigest := fields[digest]
	if hasID != hasDigest {
		return false, invalid(id, "identity and digest must be present together")
	}
	return hasID, nil
}

func validateScopes(fields map[string]Value) error {
	value, ok := fields["scopes"]
	if !ok || value.kind != KindArray || len(value.a) == 0 {
		return invalid("scopes", "at least one derived scope is required")
	}
	var prior string
	for index, item := range value.a {
		if item.kind != KindObject {
			return invalid("scopes", "scope row must be a map")
		}
		allowed := map[string]bool{"scope_kind": true, "scope_id": true, "scope_generation_u64": true, "window_kind": true, "window_start_unix_ms": true, "window_end_unix_ms_optional": true}
		for name := range item.o {
			if !allowed[name] {
				return invalid("scopes", "scope row has an unknown field")
			}
		}
		for _, name := range []string{"scope_kind", "scope_id", "scope_generation_u64", "window_kind", "window_start_unix_ms"} {
			if _, found := item.o[name]; !found {
				return invalid("scopes", "scope row has a missing field")
			}
		}
		kind, err := textField(item.o, "scope_kind")
		if err != nil {
			return err
		}
		if !contains(kind, "TENANT", "PROJECT", "WORK_ITEM", "AGREEMENT", "CUSTOMER", "PROVIDER") {
			return invalid("scope_kind", "unknown scope kind")
		}
		id, err := textField(item.o, "scope_id")
		if err != nil {
			return err
		}
		generation, err := unsignedField(item.o, "scope_generation_u64")
		if err != nil {
			return err
		}
		window, err := textField(item.o, "window_kind")
		if err != nil {
			return err
		}
		if !contains(window, "LIFETIME", "CALENDAR_HOUR", "CALENDAR_DAY", "FIXED_RANGE") {
			return invalid("window_kind", "unknown window kind")
		}
		start, err := unsignedField(item.o, "window_start_unix_ms")
		if err != nil {
			return err
		}
		end, hasEnd, err := optionalUnsignedField(item.o, "window_end_unix_ms_optional")
		if err != nil {
			return err
		}
		if window == "FIXED_RANGE" && (!hasEnd || end <= start) {
			return invalid("window_end_unix_ms_optional", "fixed range requires an end after its start")
		}
		if window != "FIXED_RANGE" && hasEnd {
			return invalid("window_end_unix_ms_optional", "only fixed ranges may carry an end")
		}
		row := fmt.Sprintf("%s\x00%s\x00%020d\x00%s\x00%020d\x00%t\x00%020d", kind, id, generation, window, start, hasEnd, end)
		if index > 0 && row <= prior {
			return invalid("scopes", "scope rows must be sorted and unique")
		}
		prior = row
	}
	return nil
}

func legalReservationTransition(to, reason string) bool {
	switch to {
	case "PRE_DISPATCH_RELEASED":
		return contains(reason, "QUEUE_FULL_BEFORE_DISPATCH", "CLIENT_CANCEL_BEFORE_DISPATCH", "DEADLINE_BEFORE_DISPATCH")
	case "DEFINITIVE_NON_BILLABLE_RELEASED":
		return reason == "PROVIDER_DEFINITIVE_NON_BILLABLE"
	case "RECONCILED":
		return reason == "PROVIDER_USAGE_RECONCILED"
	case "QUARANTINED":
		return contains(reason, "CLIENT_CANCEL_AFTER_DISPATCH", "DEADLINE_AFTER_DISPATCH", "TRANSPORT_LOST", "INVALID_PROVIDER_RESPONSE", "GATEWAY_LOST_AFTER_DISPATCH_COMMIT", "EXPIRED_WITH_UNKNOWN_OUTCOME")
	}
	return false
}

func legalDisposition(disposition, reason string) bool {
	switch disposition {
	case "PRE_DISPATCH_REJECTED":
		return contains(reason, "QUEUE_FULL", "AUTHORITY_DENIED")
	case "PRE_DISPATCH_CANCELLED":
		return reason == "CLIENT_CANCELLED"
	case "PRE_DISPATCH_DEADLINE_EXCEEDED":
		return reason == "EXECUTION_DEADLINE_EXPIRED"
	}
	return false
}

func legalTerminalOutcome(state, reason string) bool {
	switch state {
	case "DEFINITIVE_REJECT":
		return reason == "PROVIDER_DEFINITIVE_NON_BILLABLE_REJECT"
	case "COMPLETED":
		return reason == "PROVIDER_SUCCESS"
	case "AMBIGUOUS":
		return contains(reason, "CLIENT_CANCEL_AFTER_DISPATCH", "DEADLINE_AFTER_DISPATCH", "TRANSPORT_LOST", "INVALID_RESPONSE", "GATEWAY_LOST_AFTER_DISPATCH_COMMIT")
	}
	return false
}

func textField(fields map[string]Value, name string) (string, error) {
	value, ok := fields[name]
	if !ok {
		return "", fmt.Errorf("inference contract: missing required field %s", name)
	}
	if value.kind != KindText {
		return "", fmt.Errorf("inference contract: invalid type for field %s", name)
	}
	return value.s, nil
}

func unsignedField(fields map[string]Value, name string) (uint64, error) {
	value, ok := fields[name]
	if !ok {
		return 0, fmt.Errorf("inference contract: missing required field %s", name)
	}
	if value.kind != KindUnsigned {
		return 0, fmt.Errorf("inference contract: invalid type for field %s", name)
	}
	return value.u, nil
}

func optionalUnsignedField(fields map[string]Value, name string) (uint64, bool, error) {
	value, ok := fields[name]
	if !ok {
		return 0, false, nil
	}
	if value.kind != KindUnsigned {
		return 0, false, fmt.Errorf("inference contract: invalid type for field %s", name)
	}
	return value.u, true, nil
}

func takeUnsigned(fields map[string]Value, name string) (uint64, error) {
	value, err := unsignedField(fields, name)
	if err != nil {
		return 0, err
	}
	delete(fields, name)
	return value, nil
}

func requireText(fields map[string]Value, name, expected string) error {
	actual, err := textField(fields, name)
	if err != nil {
		return err
	}
	if actual != expected {
		return invalid(name, "unexpected closed-enum value")
	}
	return nil
}

func requireOneOf(fields map[string]Value, name string, values ...string) error {
	actual, err := textField(fields, name)
	if err != nil {
		return err
	}
	if !contains(actual, values...) {
		return invalid(name, "unexpected closed-enum value")
	}
	return nil
}

func contains(actual string, values ...string) bool {
	for _, value := range values {
		if actual == value {
			return true
		}
	}
	return false
}

func invalid(field, reason string) error {
	return fmt.Errorf("inference contract: invalid value for field %s: %s", field, reason)
}

func kindName(kind ValueKind) string {
	return [...]string{"unsigned", "text", "bytes", "digest32", "bool", "array", "object"}[kind]
}

func SchemaDigest() (string, error) {
	records := make([]Value, 0, len(AllRecordTypes))
	for _, recordType := range AllRecordTypes {
		fields := make([]Value, 0, len(schemas[recordType]))
		for _, spec := range schemas[recordType] {
			fields = append(fields, Object(map[string]Value{
				"kind": Text(kindName(spec.kind)), "name": Text(spec.name), "required": Bool(spec.required),
			}))
		}
		records = append(records, Object(map[string]Value{
			"digest_field": Text(digestFields[recordType]), "fields": Array(fields), "record_type": Text(string(recordType)),
		}))
	}
	payload, err := canonicalPayload(map[string]Value{"domain": Text(DigestDomain), "records": Array(records), "version": Unsigned(SchemaVersion)})
	if err != nil {
		return "", err
	}
	digest := sha256.Sum256(payload)
	return hex.EncodeToString(digest[:]), nil
}

func sortedSchemaTypes() []RecordType {
	result := append([]RecordType(nil), AllRecordTypes[:]...)
	sort.Slice(result, func(i, j int) bool { return result[i] < result[j] })
	return result
}
