package inferencecontract

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"math"
)

type encodedPair struct {
	key   []byte
	value []byte
}

func canonicalPayload(fields map[string]Value) ([]byte, error) {
	var output []byte
	if err := encodeValue(Object(fields), &output); err != nil {
		return nil, err
	}
	if len(output) > MaxRecordBytes {
		return nil, ErrRecordTooLarge
	}
	return output, nil
}

func encodeValue(value Value, output *[]byte) error {
	switch value.kind {
	case KindUnsigned:
		encodeHead(0, value.u, output)
	case KindText:
		if err := validateText(value.s, maxProviderRequestID); err != nil {
			return err
		}
		encodeHead(3, uint64(len(value.s)), output)
		*output = append(*output, value.s...)
	case KindBytes, KindDigest:
		if value.kind == KindDigest && len(value.b) != 32 {
			return errors.New("inference contract: digest must be 32 bytes")
		}
		encodeHead(2, uint64(len(value.b)), output)
		*output = append(*output, value.b...)
	case KindBool:
		if value.flag {
			*output = append(*output, 0xf5)
		} else {
			*output = append(*output, 0xf4)
		}
	case KindArray:
		encodeHead(4, uint64(len(value.a)), output)
		for _, item := range value.a {
			if err := encodeValue(item, output); err != nil {
				return err
			}
		}
	case KindObject:
		pairs := make([]encodedPair, 0, len(value.o))
		for key, item := range value.o {
			var keyBytes, valueBytes []byte
			if err := encodeValue(Text(key), &keyBytes); err != nil {
				return err
			}
			if err := encodeValue(item, &valueBytes); err != nil {
				return err
			}
			pairs = append(pairs, encodedPair{key: keyBytes, value: valueBytes})
		}
		sortEncodedPairs(pairs)
		encodeHead(5, uint64(len(pairs)), output)
		for _, pair := range pairs {
			*output = append(*output, pair.key...)
			*output = append(*output, pair.value...)
		}
	default:
		return errors.New("inference contract: unknown value kind")
	}
	return nil
}

func sortEncodedPairs(pairs []encodedPair) {
	for i := 1; i < len(pairs); i++ {
		for j := i; j > 0 && deterministicKeyCompare(pairs[j].key, pairs[j-1].key) < 0; j-- {
			pairs[j], pairs[j-1] = pairs[j-1], pairs[j]
		}
	}
}

func deterministicKeyCompare(left, right []byte) int {
	if len(left) < len(right) {
		return -1
	}
	if len(left) > len(right) {
		return 1
	}
	return bytes.Compare(left, right)
}

func encodeHead(major byte, value uint64, output *[]byte) {
	prefix := major << 5
	switch {
	case value <= 23:
		*output = append(*output, prefix|byte(value))
	case value <= math.MaxUint8:
		*output = append(*output, prefix|24, byte(value))
	case value <= math.MaxUint16:
		var raw [2]byte
		binary.BigEndian.PutUint16(raw[:], uint16(value))
		*output = append(*output, prefix|25)
		*output = append(*output, raw[:]...)
	case value <= math.MaxUint32:
		var raw [4]byte
		binary.BigEndian.PutUint32(raw[:], uint32(value))
		*output = append(*output, prefix|26)
		*output = append(*output, raw[:]...)
	default:
		var raw [8]byte
		binary.BigEndian.PutUint64(raw[:], value)
		*output = append(*output, prefix|27)
		*output = append(*output, raw[:]...)
	}
}

type decoder struct {
	data   []byte
	cursor int
}

func decodeComplete(data []byte) (Value, error) {
	decoder := decoder{data: data}
	value, err := decoder.value(0)
	if err != nil {
		return Value{}, err
	}
	if decoder.cursor != len(data) {
		return Value{}, errors.New("inference contract: trailing CBOR bytes")
	}
	return value, nil
}

func (d *decoder) value(depth int) (Value, error) {
	if depth > 16 {
		return Value{}, errors.New("inference contract: CBOR nesting limit exceeded")
	}
	initial, err := d.byte()
	if err != nil {
		return Value{}, err
	}
	major, additional := initial>>5, initial&0x1f
	switch major {
	case 0:
		value, err := d.argument(additional)
		if err != nil {
			return Value{}, err
		}
		return Unsigned(value), nil
	case 2:
		length, err := d.length(additional)
		if err != nil {
			return Value{}, err
		}
		value, err := d.take(length)
		if err != nil {
			return Value{}, err
		}
		return Bytes(value), nil
	case 3:
		length, err := d.length(additional)
		if err != nil {
			return Value{}, err
		}
		raw, err := d.take(length)
		if err != nil {
			return Value{}, err
		}
		value := string(raw)
		if err := validateText(value, maxProviderRequestID); err != nil {
			return Value{}, err
		}
		return Text(value), nil
	case 4:
		length, err := d.length(additional)
		if err != nil {
			return Value{}, err
		}
		values := make([]Value, 0, length)
		for range length {
			value, err := d.value(depth + 1)
			if err != nil {
				return Value{}, err
			}
			values = append(values, value)
		}
		return Array(values), nil
	case 5:
		length, err := d.length(additional)
		if err != nil {
			return Value{}, err
		}
		values := make(map[string]Value, length)
		var previous []byte
		for range length {
			start := d.cursor
			keyValue, err := d.value(depth + 1)
			if err != nil {
				return Value{}, err
			}
			if keyValue.kind != KindText {
				return Value{}, errors.New("inference contract: CBOR map keys must be text")
			}
			encodedKey := append([]byte(nil), d.data[start:d.cursor]...)
			if previous != nil && deterministicKeyCompare(previous, encodedKey) >= 0 {
				return Value{}, errors.New("inference contract: CBOR map keys are duplicate or not deterministic")
			}
			previous = encodedKey
			item, err := d.value(depth + 1)
			if err != nil {
				return Value{}, err
			}
			if _, exists := values[keyValue.s]; exists {
				return Value{}, errors.New("inference contract: duplicate CBOR map key")
			}
			values[keyValue.s] = item
		}
		return Object(values), nil
	case 1, 6:
		return Value{}, errors.New("inference contract: negative integers and CBOR tags are forbidden")
	case 7:
		if additional == 20 {
			return Bool(false), nil
		}
		if additional == 21 {
			return Bool(true), nil
		}
		return Value{}, errors.New("inference contract: null, floats, and simple CBOR values are forbidden")
	default:
		return Value{}, errors.New("inference contract: unsupported CBOR major type")
	}
}

func (d *decoder) length(additional byte) (int, error) {
	value, err := d.argument(additional)
	if err != nil {
		return 0, err
	}
	if value > uint64(len(d.data)) || value > uint64(MaxRecordBytes) {
		return 0, ErrRecordTooLarge
	}
	return int(value), nil
}

func (d *decoder) argument(additional byte) (uint64, error) {
	switch {
	case additional <= 23:
		return uint64(additional), nil
	case additional == 24:
		value, err := d.byte()
		if err != nil {
			return 0, err
		}
		if value < 24 {
			return 0, errors.New("inference contract: non-shortest CBOR integer or length")
		}
		return uint64(value), nil
	case additional == 25:
		raw, err := d.take(2)
		if err != nil {
			return 0, err
		}
		value := uint64(binary.BigEndian.Uint16(raw))
		if value <= math.MaxUint8 {
			return 0, errors.New("inference contract: non-shortest CBOR integer or length")
		}
		return value, nil
	case additional == 26:
		raw, err := d.take(4)
		if err != nil {
			return 0, err
		}
		value := uint64(binary.BigEndian.Uint32(raw))
		if value <= math.MaxUint16 {
			return 0, errors.New("inference contract: non-shortest CBOR integer or length")
		}
		return value, nil
	case additional == 27:
		raw, err := d.take(8)
		if err != nil {
			return 0, err
		}
		value := binary.BigEndian.Uint64(raw)
		if value <= math.MaxUint32 {
			return 0, errors.New("inference contract: non-shortest CBOR integer or length")
		}
		return value, nil
	default:
		return 0, errors.New("inference contract: indefinite or reserved CBOR length")
	}
}

func (d *decoder) byte() (byte, error) {
	if d.cursor >= len(d.data) {
		return 0, errors.New("inference contract: truncated CBOR input")
	}
	value := d.data[d.cursor]
	d.cursor++
	return value, nil
}

func (d *decoder) take(length int) ([]byte, error) {
	if length < 0 || d.cursor > len(d.data)-length {
		return nil, errors.New("inference contract: truncated CBOR input")
	}
	value := append([]byte(nil), d.data[d.cursor:d.cursor+length]...)
	d.cursor += length
	return value, nil
}

func parseDigestHex(value string) ([32]byte, error) {
	var digest [32]byte
	raw, err := hexDecode(value)
	if err != nil || len(raw) != len(digest) {
		return digest, fmt.Errorf("inference contract: invalid digest hex")
	}
	copy(digest[:], raw)
	return digest, nil
}

func hexDecode(value string) ([]byte, error) {
	if len(value)%2 != 0 {
		return nil, errors.New("odd hex length")
	}
	decoded := make([]byte, len(value)/2)
	for index := range decoded {
		high, ok := hexNibble(value[index*2])
		if !ok {
			return nil, errors.New("invalid hex")
		}
		low, ok := hexNibble(value[index*2+1])
		if !ok {
			return nil, errors.New("invalid hex")
		}
		decoded[index] = high<<4 | low
	}
	return decoded, nil
}

func hexNibble(value byte) (byte, bool) {
	switch {
	case value >= '0' && value <= '9':
		return value - '0', true
	case value >= 'a' && value <= 'f':
		return value - 'a' + 10, true
	default:
		return 0, false
	}
}
