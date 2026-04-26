package analyzer

import (
	"context"
	"encoding/json"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"

	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/gateway"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/persistence"
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

func TestAnalyzeBehavior(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		resp := gateway.ChatResponse{
			Content:    `{"habits":["morgendlicher Kaffee","Code-Review vor Mittag"],"interaction_style":"proaktiv","decision_style":"schnell","anomalies":[]}`,
			Model:      "qwen3:7b",
			TokensUsed: 250,
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
		"Ich hole mir erstmal einen Kaffee.",
		"Dann schaue ich mir die Pull Requests an.",
		"Der Code sieht gut aus, ich merge das.",
	}

	result, err := a.AnalyzeBehavior(context.Background(), "AGENT-03", "developer", messages, 1000)
	if err != nil {
		t.Fatalf("AnalyzeBehavior: %v", err)
	}

	if len(result.Habits) == 0 {
		t.Error("expected habits, got none")
	}
	if result.InteractionStyle == "" {
		t.Error("expected interaction_style, got empty")
	}
	if result.DecisionStyle == "" {
		t.Error("expected decision_style, got empty")
	}

	entries, err := evol.GetByAgent("AGENT-03")
	if err != nil {
		t.Fatalf("GetByAgent: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected 1 evolution entry, got %d", len(entries))
	}
	if entries[0].Field != "behavioral_notes" {
		t.Errorf("field = %q, want behavioral_notes", entries[0].Field)
	}
}

func TestAnalyzeNarrative(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		resp := gateway.ChatResponse{
			Content:    `{"mood":"positiv","turning_points":["Kunde hat Feedback gegeben"],"theme":"Produktiver Arbeitstag mit positivem Kundenfeedback","arc_summary":"Ruhiger Start, produktive Mitte mit Kundenkontakt, zufriedenes Ende."}`,
			Model:      "qwen3:7b",
			TokensUsed: 300,
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
		"Guten Morgen, ich starte mit dem neuen Feature.",
		"Der Kunde hat positives Feedback zum Prototyp gegeben!",
		"Ich schliesse die Aufgabe ab, guter Tag heute.",
	}

	result, err := a.AnalyzeNarrative(context.Background(), "AGENT-07", "designer", messages, 2000)
	if err != nil {
		t.Fatalf("AnalyzeNarrative: %v", err)
	}

	if result.Mood == "" {
		t.Error("expected mood, got empty")
	}
	if result.Theme == "" {
		t.Error("expected theme, got empty")
	}
	if result.ArcSummary == "" {
		t.Error("expected arc_summary, got empty")
	}

	entries, err := evol.GetByAgent("AGENT-07")
	if err != nil {
		t.Fatalf("GetByAgent: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected 1 evolution entry, got %d", len(entries))
	}
	if entries[0].Field != "narrative_arc" {
		t.Errorf("field = %q, want narrative_arc", entries[0].Field)
	}
}

func TestAnalyzeRelationships(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		resp := gateway.ChatResponse{
			Content:    `{"relationships":[{"colleague":"Lisa","quality":"positiv"},{"colleague":"Andreas","quality":"neutral"}],"collaboration_partners":["Lisa"],"conflicts":[],"team_role":"unterstuetzend"}`,
			Model:      "qwen3:7b",
			TokensUsed: 280,
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
		"Lisa und ich arbeiten am Header-Design.",
		"Andreas hat den Backend-Code fertig, ich schaue mir das an.",
		"Gute Zusammenarbeit mit Lisa heute, das Ergebnis passt.",
	}

	result, err := a.AnalyzeRelationships(context.Background(), "AGENT-07", "designer", messages, 3000)
	if err != nil {
		t.Fatalf("AnalyzeRelationships: %v", err)
	}

	if len(result.Relationships) == 0 {
		t.Error("expected relationships, got none")
	}
	if result.TeamRole == "" {
		t.Error("expected team_role, got empty")
	}

	entries, err := evol.GetByAgent("AGENT-07")
	if err != nil {
		t.Fatalf("GetByAgent: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected 1 evolution entry, got %d", len(entries))
	}
	if entries[0].Field != "relationship_dynamics" {
		t.Errorf("field = %q, want relationship_dynamics", entries[0].Field)
	}
}

func TestAnalyzeBehaviorEmptyMessages(t *testing.T) {
	a := New(nil, nil, slog.Default())

	_, err := a.AnalyzeBehavior(context.Background(), "AGENT-01", "dev", nil, 0)
	if err == nil {
		t.Error("expected error for empty messages")
	}
}

func TestAnalyzeNarrativeEmptyMessages(t *testing.T) {
	a := New(nil, nil, slog.Default())

	_, err := a.AnalyzeNarrative(context.Background(), "AGENT-01", "dev", nil, 0)
	if err == nil {
		t.Error("expected error for empty messages")
	}
}

func TestAnalyzeRelationshipsEmptyMessages(t *testing.T) {
	a := New(nil, nil, slog.Default())

	_, err := a.AnalyzeRelationships(context.Background(), "AGENT-01", "dev", nil, 0)
	if err == nil {
		t.Error("expected error for empty messages")
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
