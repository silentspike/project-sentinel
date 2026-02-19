package analyzer

import (
	"context"
	"encoding/json"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"

	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/gateway"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/persistence"
)

func TestAnalyzeVoice(t *testing.T) {
	// Mock gateway returns valid voice analysis JSON
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		resp := gateway.ChatResponse{
			Content:    `{"phrases":["Guten Morgen","Ich schau mir das an"],"sentence_style":"mittel","formality":0.6}`,
			Model:      "qwen3:7b",
			TokensUsed: 200,
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(resp)
	}))
	defer server.Close()

	client := gateway.NewClient(gateway.ClientConfig{URL: server.URL})

	evolPath := filepath.Join(t.TempDir(), "evolution.db")
	evol, err := persistence.OpenEvolution(evolPath)
	if err != nil {
		t.Fatalf("OpenEvolution: %v", err)
	}
	defer func() { _ = evol.Close() }()

	a := New(client, evol, slog.Default())

	messages := []string{
		"Guten Morgen, ich schau mir das mal an.",
		"Das Design sieht gut aus, ich schau mir das an.",
		"Wir koennen das so umsetzen.",
	}

	result, err := a.AnalyzeVoice(context.Background(), "AGENT-07", "designer", messages, 1000)
	if err != nil {
		t.Fatalf("AnalyzeVoice: %v", err)
	}

	if len(result.Phrases) == 0 {
		t.Error("expected phrases, got none")
	}
	if result.SentenceStyle == "" {
		t.Error("expected sentence_style, got empty")
	}
	if result.Formality <= 0 || result.Formality > 1 {
		t.Errorf("formality = %f, want in (0, 1]", result.Formality)
	}

	// Verify persistence
	entries, err := evol.GetByAgent("AGENT-07")
	if err != nil {
		t.Fatalf("GetByAgent: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected 1 evolution entry, got %d", len(entries))
	}
	if entries[0].Field != "voice_style" {
		t.Errorf("field = %q, want voice_style", entries[0].Field)
	}
}

func TestAnalyzeVoiceEmptyMessages(t *testing.T) {
	a := New(nil, nil, slog.Default())

	_, err := a.AnalyzeVoice(context.Background(), "AGENT-01", "dev", nil, 0)
	if err == nil {
		t.Error("expected error for empty messages")
	}
}

func TestAnalyzeVoiceInvalidJSON(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		resp := gateway.ChatResponse{
			Content: "This is not JSON at all",
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(resp)
	}))
	defer server.Close()

	client := gateway.NewClient(gateway.ClientConfig{URL: server.URL})

	evolPath := filepath.Join(t.TempDir(), "evolution.db")
	evol, _ := persistence.OpenEvolution(evolPath)
	defer func() { _ = evol.Close() }()

	a := New(client, evol, slog.Default())

	_, err := a.AnalyzeVoice(context.Background(), "AGENT-01", "dev", []string{"hello"}, 0)
	if err == nil {
		t.Error("expected error for invalid JSON response")
	}
}
