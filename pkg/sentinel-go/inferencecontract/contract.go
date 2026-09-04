// Package inferencecontract implements the version-1 cross-language inference
// authority records. The codec accepts only the closed deterministic-CBOR
// subset used by Sentinel's Rust authority boundary.
package inferencecontract

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"sort"
	"unicode/utf8"

	"golang.org/x/text/unicode/norm"
)

const (
	SchemaVersion        uint64 = 1
	DigestDomain                = "sentinel.inference.control"
	MaxRecordBytes              = 64 * 1024
	maxAuthorityText            = 128
	maxProviderRequestID        = 512
)

type RecordType string

const (
	AdmissionIntent             RecordType = "AdmissionIntentV1"
	BudgetReservation           RecordType = "BudgetReservationV1"
	BudgetReservationTransition RecordType = "BudgetReservationTransitionV1"
	BudgetExemption             RecordType = "BudgetExemptionV1"
	InferenceAdmission          RecordType = "InferenceAdmissionV1"
	AdmissionDisposition        RecordType = "AdmissionDispositionV1"
	InferenceAuthorityPort      RecordType = "InferenceAuthorityPortV1"
	ProviderDispatchReceipt     RecordType = "ProviderDispatchReceiptV1"
	ProviderAttemptOutcome      RecordType = "ProviderAttemptOutcomeV1"
	UsageOutcome                RecordType = "UsageOutcomeV1"
	ProviderCapabilities        RecordType = "ProviderCapabilitiesV1"
)

var AllRecordTypes = [...]RecordType{
	AdmissionIntent,
	BudgetReservation,
	BudgetReservationTransition,
	BudgetExemption,
	InferenceAdmission,
	AdmissionDisposition,
	InferenceAuthorityPort,
	ProviderDispatchReceipt,
	ProviderAttemptOutcome,
	UsageOutcome,
	ProviderCapabilities,
}

var AuthorityPortMethodsV1 = [...]string{
	"RESERVE_OR_EXEMPT",
	"FINALIZE_ADMISSION",
	"WIN_PRE_DISPATCH_DISPOSITION",
	"BEGIN_DISPATCH",
	"COMMIT_ATTEMPT_OUTCOME",
	"RECONCILE_USAGE",
}

type AuthorityResultV1 string

const (
	ResultCommitted           AuthorityResultV1 = "COMMITTED"
	ResultReplayedReadback    AuthorityResultV1 = "REPLAYED_READBACK"
	ResultDenied              AuthorityResultV1 = "DENIED"
	ResultIdempotencyConflict AuthorityResultV1 = "IDEMPOTENCY_CONFLICT"
	ResultStalePredecessor    AuthorityResultV1 = "STALE_PREDECESSOR"
	ResultIllegalTransition   AuthorityResultV1 = "ILLEGAL_TRANSITION"
	ResultUnknownVersion      AuthorityResultV1 = "UNKNOWN_VERSION"
	ResultUnknownMethod       AuthorityResultV1 = "UNKNOWN_METHOD"
	ResultUnauthorized        AuthorityResultV1 = "UNAUTHORIZED"
	ResultUnavailable         AuthorityResultV1 = "UNAVAILABLE"
)

var AllAuthorityResultsV1 = [...]AuthorityResultV1{
	ResultCommitted,
	ResultReplayedReadback,
	ResultDenied,
	ResultIdempotencyConflict,
	ResultStalePredecessor,
	ResultIllegalTransition,
	ResultUnknownVersion,
	ResultUnknownMethod,
	ResultUnauthorized,
	ResultUnavailable,
}

type ValueKind uint8

const (
	KindUnsigned ValueKind = iota
	KindText
	KindBytes
	KindDigest
	KindBool
	KindArray
	KindObject
)

type Value struct {
	kind ValueKind
	u    uint64
	s    string
	b    []byte
	flag bool
	a    []Value
	o    map[string]Value
}

func Unsigned(value uint64) Value { return Value{kind: KindUnsigned, u: value} }
func Text(value string) Value     { return Value{kind: KindText, s: value} }
func Bytes(value []byte) Value    { return Value{kind: KindBytes, b: append([]byte(nil), value...)} }
func Digest(value [32]byte) Value {
	return Value{kind: KindDigest, b: append([]byte(nil), value[:]...)}
}
func Bool(value bool) Value     { return Value{kind: KindBool, flag: value} }
func Array(value []Value) Value { return Value{kind: KindArray, a: append([]Value(nil), value...)} }
func Object(value map[string]Value) Value {
	return Value{kind: KindObject, o: cloneFields(value)}
}

func (v Value) Kind() ValueKind          { return v.kind }
func (v Value) Uint64() uint64           { return v.u }
func (v Value) String() string           { return v.s }
func (v Value) ByteSlice() []byte        { return append([]byte(nil), v.b...) }
func (v Value) Boolean() bool            { return v.flag }
func (v Value) Values() []Value          { return append([]Value(nil), v.a...) }
func (v Value) Fields() map[string]Value { return cloneFields(v.o) }

type Record struct {
	recordType RecordType
	fields     map[string]Value
	digest     [32]byte
}

// AuthorityResponseV1 is the strict response from the Rust inference
// authority. The corresponding authenticated request method is validation
// context, so only a fresh BEGIN_DISPATCH commit can authorize provider I/O.
type AuthorityResponseV1 struct {
	result                 AuthorityResultV1
	committedOperationID   string
	committedPayloadDigest *[32]byte
	aggregateState         string
	providerIOAuthorized   bool
}

func NewAuthorityResponseV1(
	requestMethod string,
	result AuthorityResultV1,
	committedOperationID string,
	committedPayloadDigest *[32]byte,
	aggregateState string,
	providerIOAuthorized bool,
) (*AuthorityResponseV1, error) {
	if err := validateAuthorityResponse(requestMethod, result, committedOperationID, committedPayloadDigest, aggregateState, providerIOAuthorized); err != nil {
		return nil, err
	}
	var digestCopy *[32]byte
	if committedPayloadDigest != nil {
		value := *committedPayloadDigest
		digestCopy = &value
	}
	return &AuthorityResponseV1{
		result:                 result,
		committedOperationID:   committedOperationID,
		committedPayloadDigest: digestCopy,
		aggregateState:         aggregateState,
		providerIOAuthorized:   providerIOAuthorized,
	}, nil
}

func DecodeAuthorityResponseV1(requestMethod string, data []byte) (*AuthorityResponseV1, error) {
	if len(data) > MaxRecordBytes {
		return nil, ErrRecordTooLarge
	}
	value, err := decodeComplete(data)
	if err != nil {
		return nil, err
	}
	if value.kind != KindObject {
		return nil, errors.New("inference contract: root must be a map")
	}
	fields := value.o
	version, err := takeUnsigned(fields, "version")
	if err != nil {
		return nil, err
	}
	if version != SchemaVersion {
		return nil, fmt.Errorf("inference contract: unsupported schema version %d", version)
	}
	allowed := map[string]bool{
		"result":                            true,
		"committed_operation_id_optional":   true,
		"committed_payload_digest_optional": true,
		"aggregate_state_optional":          true,
		"provider_io_authorized":            true,
	}
	for name := range fields {
		if !allowed[name] {
			return nil, fmt.Errorf("inference contract: unknown field %s", name)
		}
	}
	resultText, err := textField(fields, "result")
	if err != nil {
		return nil, err
	}
	result := AuthorityResultV1(resultText)
	committedOperationID, err := optionalTextValue(fields, "committed_operation_id_optional")
	if err != nil {
		return nil, err
	}
	committedPayloadDigest, err := optionalDigestValue(fields, "committed_payload_digest_optional")
	if err != nil {
		return nil, err
	}
	aggregateState, err := optionalTextValue(fields, "aggregate_state_optional")
	if err != nil {
		return nil, err
	}
	providerValue, ok := fields["provider_io_authorized"]
	if !ok {
		return nil, errors.New("inference contract: missing required field provider_io_authorized")
	}
	if providerValue.kind != KindBool {
		return nil, errors.New("inference contract: invalid type for field provider_io_authorized")
	}
	return NewAuthorityResponseV1(requestMethod, result, committedOperationID, committedPayloadDigest, aggregateState, providerValue.flag)
}

func (r *AuthorityResponseV1) Result() AuthorityResultV1  { return r.result }
func (r *AuthorityResponseV1) ProviderIOAuthorized() bool { return r.providerIOAuthorized }

func (r *AuthorityResponseV1) CanonicalWirePayload() ([]byte, error) {
	fields := map[string]Value{
		"version":                Unsigned(SchemaVersion),
		"result":                 Text(string(r.result)),
		"provider_io_authorized": Bool(r.providerIOAuthorized),
	}
	if r.committedOperationID != "" {
		fields["committed_operation_id_optional"] = Text(r.committedOperationID)
	}
	if r.committedPayloadDigest != nil {
		fields["committed_payload_digest_optional"] = Digest(*r.committedPayloadDigest)
	}
	if r.aggregateState != "" {
		fields["aggregate_state_optional"] = Text(r.aggregateState)
	}
	return canonicalPayload(fields)
}

func (r *AuthorityResponseV1) Digest() ([32]byte, error) {
	var zero [32]byte
	payload, err := r.CanonicalWirePayload()
	if err != nil {
		return zero, err
	}
	return digestNamedPayload("InferenceAuthorityResponseV1", payload)
}

func (r *AuthorityResponseV1) DigestHex() (string, error) {
	digest, err := r.Digest()
	if err != nil {
		return "", err
	}
	return hex.EncodeToString(digest[:]), nil
}

func NewRecord(recordType RecordType, fields map[string]Value) (*Record, error) {
	fields = cloneFields(fields)
	if err := validateFields(recordType, fields); err != nil {
		return nil, err
	}
	digest, err := digestRecord(recordType, fields)
	if err != nil {
		return nil, err
	}
	return &Record{recordType: recordType, fields: fields, digest: digest}, nil
}

func DecodeRecord(recordType RecordType, data []byte) (*Record, error) {
	if len(data) > MaxRecordBytes {
		return nil, ErrRecordTooLarge
	}
	value, err := decodeComplete(data)
	if err != nil {
		return nil, err
	}
	if value.kind != KindObject {
		return nil, errors.New("inference contract: root must be a map")
	}
	fields := value.o
	version, err := takeUnsigned(fields, "version")
	if err != nil {
		return nil, err
	}
	if version != SchemaVersion {
		return nil, fmt.Errorf("inference contract: unsupported schema version %d", version)
	}
	digestName, err := ownDigestField(recordType)
	if err != nil {
		return nil, err
	}
	digestValue, ok := fields[digestName]
	if !ok {
		return nil, fmt.Errorf("inference contract: missing required field %s", digestName)
	}
	delete(fields, digestName)
	if digestValue.kind != KindBytes || len(digestValue.b) != sha256.Size {
		return nil, fmt.Errorf("inference contract: invalid type for field %s", digestName)
	}
	var digest [32]byte
	copy(digest[:], digestValue.b)
	if err := normalizeDigestFields(recordType, fields); err != nil {
		return nil, err
	}
	if err := validateFields(recordType, fields); err != nil {
		return nil, err
	}
	want, err := digestRecord(recordType, fields)
	if err != nil {
		return nil, err
	}
	if want != digest {
		return nil, ErrDigestMismatch
	}
	return &Record{recordType: recordType, fields: fields, digest: digest}, nil
}

func (r *Record) RecordType() RecordType                { return r.recordType }
func (r *Record) Fields() map[string]Value              { return cloneFields(r.fields) }
func (r *Record) Digest() [32]byte                      { return r.digest }
func (r *Record) DigestHex() string                     { return hex.EncodeToString(r.digest[:]) }
func (r *Record) CanonicalHashPayload() ([]byte, error) { return canonicalHashPayload(r.fields) }

func (r *Record) CanonicalWirePayload() ([]byte, error) {
	fields := cloneFields(r.fields)
	fields["version"] = Unsigned(SchemaVersion)
	digestName, err := ownDigestField(r.recordType)
	if err != nil {
		return nil, err
	}
	fields[digestName] = Digest(r.digest)
	return canonicalPayload(fields)
}

var (
	ErrDigestMismatch = errors.New("inference contract: digest does not match canonical bytes")
	ErrRecordTooLarge = errors.New("inference contract: record exceeds its size bound")
)

func digestRecord(recordType RecordType, fields map[string]Value) ([32]byte, error) {
	var zero [32]byte
	payload, err := canonicalHashPayload(fields)
	if err != nil {
		return zero, err
	}
	return digestNamedPayload(string(recordType), payload)
}

func digestNamedPayload(recordType string, payload []byte) ([32]byte, error) {
	var zero [32]byte
	typeBytes := []byte(recordType)
	if len(typeBytes) > int(^uint16(0)) || len(payload) > int(^uint32(0)) {
		return zero, ErrRecordTooLarge
	}
	preimage := make([]byte, 0, len(DigestDomain)+1+2+len(typeBytes)+2+4+len(payload))
	preimage = append(preimage, []byte(DigestDomain)...)
	preimage = append(preimage, 0)
	var scratch [4]byte
	binary.BigEndian.PutUint16(scratch[:2], uint16(len(typeBytes)))
	preimage = append(preimage, scratch[:2]...)
	preimage = append(preimage, typeBytes...)
	binary.BigEndian.PutUint16(scratch[:2], uint16(SchemaVersion))
	preimage = append(preimage, scratch[:2]...)
	binary.BigEndian.PutUint32(scratch[:], uint32(len(payload)))
	preimage = append(preimage, scratch[:]...)
	preimage = append(preimage, payload...)
	return sha256.Sum256(preimage), nil
}

func validateAuthorityResponse(requestMethod string, result AuthorityResultV1, operationID string, payloadDigest *[32]byte, aggregateState string, providerIOAuthorized bool) error {
	if !contains(requestMethod, AuthorityPortMethodsV1[:]...) {
		return invalid("method", "unknown authority-port method")
	}
	if !contains(string(result), authorityResultStrings()...) {
		return invalid("result", "unknown authority-port result")
	}
	if (operationID == "") != (payloadDigest == nil) {
		return invalid("committed_operation_id_optional", "committed operation and payload digest must be paired")
	}
	isReadback := result == ResultCommitted || result == ResultReplayedReadback
	if isReadback != (operationID != "") {
		return invalid("committed_operation_id_optional", "only committed readbacks carry committed identity")
	}
	if operationID != "" {
		if err := validateText(operationID, maxAuthorityText); err != nil {
			return err
		}
	}
	if aggregateState != "" {
		if err := validateText(aggregateState, maxAuthorityText); err != nil {
			return err
		}
		if !isASCII(aggregateState) {
			return invalid("aggregate_state_optional", "aggregate state must be ASCII")
		}
	}
	if providerIOAuthorized && (requestMethod != "BEGIN_DISPATCH" || result != ResultCommitted) {
		return invalid("provider_io_authorized", "only a fresh BEGIN_DISPATCH commit authorizes provider I/O")
	}
	return nil
}

func authorityResultStrings() []string {
	values := make([]string, 0, len(AllAuthorityResultsV1))
	for _, result := range AllAuthorityResultsV1 {
		values = append(values, string(result))
	}
	return values
}

func optionalTextValue(fields map[string]Value, name string) (string, error) {
	value, ok := fields[name]
	if !ok {
		return "", nil
	}
	if value.kind != KindText {
		return "", fmt.Errorf("inference contract: invalid type for field %s", name)
	}
	if err := validateText(value.s, maxAuthorityText); err != nil {
		return "", err
	}
	return value.s, nil
}

func optionalDigestValue(fields map[string]Value, name string) (*[32]byte, error) {
	value, ok := fields[name]
	if !ok {
		return nil, nil
	}
	if (value.kind != KindBytes && value.kind != KindDigest) || len(value.b) != sha256.Size {
		return nil, fmt.Errorf("inference contract: invalid type for field %s", name)
	}
	var digest [32]byte
	copy(digest[:], value.b)
	return &digest, nil
}

func canonicalHashPayload(fields map[string]Value) ([]byte, error) {
	payload := cloneFields(fields)
	payload["version"] = Unsigned(SchemaVersion)
	return canonicalPayload(payload)
}

func validateFields(recordType RecordType, fields map[string]Value) error {
	specs, ok := schemas[recordType]
	if !ok {
		return fmt.Errorf("inference contract: unknown record type %s", recordType)
	}
	for _, spec := range specs {
		value, found := fields[spec.name]
		if !found {
			if spec.required {
				return fmt.Errorf("inference contract: missing required field %s", spec.name)
			}
			continue
		}
		if value.kind != spec.kind {
			return fmt.Errorf("inference contract: invalid type for field %s", spec.name)
		}
		if err := validateValue(spec.name, value); err != nil {
			return err
		}
	}
	for name := range fields {
		if !hasField(specs, name) {
			return fmt.Errorf("inference contract: unknown field %s", name)
		}
	}
	return validateRelations(recordType, fields)
}

func validateValue(field string, value Value) error {
	switch value.kind {
	case KindText:
		max := maxAuthorityText
		if field == "provider_request_id_optional" {
			max = maxProviderRequestID
		}
		if value.s == "" || len(value.s) > max || !utf8.ValidString(value.s) || !norm.NFC.IsNormalString(value.s) {
			return fmt.Errorf("inference contract: field %s must be non-empty bounded NFC text", field)
		}
		if field != "provider_request_id_optional" && !isASCII(value.s) {
			return fmt.Errorf("inference contract: field %s must be ASCII", field)
		}
	case KindBytes:
		if len(value.b) == 0 {
			return fmt.Errorf("inference contract: field %s byte payload must not be empty", field)
		}
	case KindDigest:
		if len(value.b) != sha256.Size {
			return fmt.Errorf("inference contract: field %s digest must be 32 bytes", field)
		}
	case KindArray:
		for _, nested := range value.a {
			if err := validateNested(nested); err != nil {
				return err
			}
		}
	case KindObject:
		for name, nested := range value.o {
			if err := validateText(name, maxProviderRequestID); err != nil {
				return err
			}
			if err := validateNested(nested); err != nil {
				return err
			}
		}
	}
	return nil
}

func validateNested(value Value) error {
	switch value.kind {
	case KindText:
		return validateText(value.s, maxProviderRequestID)
	case KindBytes:
		if len(value.b) == 0 {
			return errors.New("inference contract: empty nested bytes are forbidden")
		}
	case KindArray:
		for _, nested := range value.a {
			if err := validateNested(nested); err != nil {
				return err
			}
		}
	case KindObject:
		for name, nested := range value.o {
			if err := validateText(name, maxProviderRequestID); err != nil {
				return err
			}
			if err := validateNested(nested); err != nil {
				return err
			}
		}
	}
	return nil
}

func validateText(value string, max int) error {
	if value == "" || len(value) > max || !utf8.ValidString(value) || !norm.NFC.IsNormalString(value) {
		return errors.New("inference contract: text must be non-empty bounded NFC UTF-8")
	}
	return nil
}

func cloneFields(fields map[string]Value) map[string]Value {
	copy := make(map[string]Value, len(fields))
	for key, value := range fields {
		copy[key] = cloneValue(value)
	}
	return copy
}

func cloneValue(value Value) Value {
	switch value.kind {
	case KindBytes, KindDigest:
		value.b = append([]byte(nil), value.b...)
	case KindArray:
		value.a = append([]Value(nil), value.a...)
		for index := range value.a {
			value.a[index] = cloneValue(value.a[index])
		}
	case KindObject:
		value.o = cloneFields(value.o)
	}
	return value
}

func normalizeDigestFields(recordType RecordType, fields map[string]Value) error {
	specs, ok := schemas[recordType]
	if !ok {
		return fmt.Errorf("inference contract: unknown record type %s", recordType)
	}
	for _, spec := range specs {
		value, found := fields[spec.name]
		if found && spec.kind == KindDigest && value.kind == KindBytes && len(value.b) == sha256.Size {
			value.kind = KindDigest
			fields[spec.name] = value
		}
	}
	return nil
}

func hasField(specs []fieldSpec, name string) bool {
	for _, spec := range specs {
		if spec.name == name {
			return true
		}
	}
	return false
}

func isASCII(value string) bool {
	for index := 0; index < len(value); index++ {
		if value[index] > 0x7f {
			return false
		}
	}
	return true
}

func sortedKeys(fields map[string]Value) []string {
	keys := make([]string, 0, len(fields))
	for key := range fields {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}
