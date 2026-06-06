package service

import (
	"testing"

	"github.com/nats-io/nats.go"
)

func TestExtractSimTickFromHeader(t *testing.T) {
	headers := nats.Header{}
	headers.Set("X-Tick", "2009340")

	tick, ok := extractSimTick(headers, map[string]any{"tick": float64(1)})
	if !ok {
		t.Fatal("expected header tick to parse")
	}
	if tick != 2009340 {
		t.Fatalf("tick = %d, want 2009340", tick)
	}
}

func TestExtractSimTickFromPayloadFallback(t *testing.T) {
	tick, ok := extractSimTick(nats.Header{}, map[string]any{"sim_tick": float64(2009341)})
	if !ok {
		t.Fatal("expected payload tick to parse")
	}
	if tick != 2009341 {
		t.Fatalf("tick = %d, want 2009341", tick)
	}
}

func TestExtractSimTickRejectsLegacyMilliseconds(t *testing.T) {
	headers := nats.Header{}
	headers.Set("X-Tick", "1780475609399")

	if tick, ok := extractSimTick(headers, map[string]any{}); ok {
		t.Fatalf("legacy millisecond tick parsed as valid: %d", tick)
	}
}

func TestExtractSimTickRejectsFractionalPayloadTick(t *testing.T) {
	if tick, ok := extractSimTick(nats.Header{}, map[string]any{"tick": 12.5}); ok {
		t.Fatalf("fractional payload tick parsed as valid: %d", tick)
	}
}
