package apicp

import (
	"testing"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/extraction"
)

func TestBuildResponseSignatureUsesRoomForWork(t *testing.T) {
	sigA := BuildResponseSignature([]extraction.ExtractedAction{{
		Type:    "work",
		Target:  "entwicklungsbuero-1",
		Emotion: "neutral",
	}}, "buero-dev-1", "A")
	sigB := BuildResponseSignature([]extraction.ExtractedAction{{
		Type:    "work",
		Target:  "—",
		Emotion: "neutral",
	}}, "buero-dev-1", "B")

	if sigA != sigB {
		t.Fatalf("work signatures differ: %q vs %q", sigA, sigB)
	}
	if want := "work|buero-dev-1|neutral"; sigA != want {
		t.Fatalf("signature = %q, want %q", sigA, want)
	}
}

func TestBuildResponseSignatureFallsBackToNormalizedText(t *testing.T) {
	got := BuildResponseSignature(nil, "", " Hallo   Welt \n\n Test ")
	if want := "Hallo Welt Test"; got != want {
		t.Fatalf("signature = %q, want %q", got, want)
	}
}
