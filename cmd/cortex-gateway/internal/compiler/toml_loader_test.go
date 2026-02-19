package compiler

import (
	"os"
	"path/filepath"
	"testing"
)

const testAgentTOML = `[identity]
id = 1
name = "Thomas Mueller"
role = "CEO"
department = "Geschaeftsfuehrung"
shift_set = 1

[personality]
openness = 0.8
conscientiousness = 0.9
extraversion = 0.7
agreeableness = 0.6
neuroticism = 0.3
caffeine_tolerance = 0.7
morning_person = true

[background]
bio = "Thomas leitet seit 5 Jahren die Agentur."
quirks = ["trinkt nur Espresso", "laeuft immer barfuss"]

[preferences]
favorite_room = "buero-ceo"
coffee_preference = "Espresso"
lunch_time = "12:30"
`

func setupTestAgent(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "AGENT-01-THOMAS-CEO.toml")
	if err := os.WriteFile(path, []byte(testAgentTOML), 0600); err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestTOMLLoader_Load(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)

	dna, err := loader.Load(1)
	if err != nil {
		t.Fatalf("Load() error: %v", err)
	}

	if dna.Identity.Name != "Thomas Mueller" {
		t.Errorf("Name = %q, want %q", dna.Identity.Name, "Thomas Mueller")
	}
	if dna.Identity.Role != "CEO" {
		t.Errorf("Role = %q, want %q", dna.Identity.Role, "CEO")
	}
	if dna.Personality.Openness != 0.8 {
		t.Errorf("Openness = %f, want 0.8", dna.Personality.Openness)
	}
	if dna.Background.Bio != "Thomas leitet seit 5 Jahren die Agentur." {
		t.Errorf("Bio = %q", dna.Background.Bio)
	}
	if len(dna.Background.Quirks) != 2 {
		t.Errorf("Quirks count = %d, want 2", len(dna.Background.Quirks))
	}
}

func TestTOMLLoader_Cache(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)

	dna1, err := loader.Load(1)
	if err != nil {
		t.Fatalf("first Load() error: %v", err)
	}

	dna2, err := loader.Load(1)
	if err != nil {
		t.Fatalf("second Load() error: %v", err)
	}

	// Same pointer = served from cache
	if dna1 != dna2 {
		t.Error("second Load() should return cached pointer")
	}
}

func TestTOMLLoader_Invalidate(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)

	dna1, err := loader.Load(1)
	if err != nil {
		t.Fatalf("Load() error: %v", err)
	}

	loader.Invalidate(1)

	dna2, err := loader.Load(1)
	if err != nil {
		t.Fatalf("Load() after invalidate error: %v", err)
	}

	// Different pointer after invalidation
	if dna1 == dna2 {
		t.Error("Load() after Invalidate should return fresh data")
	}
}

func TestTOMLLoader_InvalidateAll(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)

	_, err := loader.Load(1)
	if err != nil {
		t.Fatalf("Load() error: %v", err)
	}

	loader.InvalidateAll()

	// Load again to verify cache was cleared
	dna, err := loader.Load(1)
	if err != nil {
		t.Fatalf("Load() after InvalidateAll error: %v", err)
	}
	if dna.Identity.Name != "Thomas Mueller" {
		t.Error("reloaded data should be correct")
	}
}

func TestTOMLLoader_MissingAgent(t *testing.T) {
	dir := t.TempDir()
	loader := NewTOMLLoader(dir)

	_, err := loader.Load(99)
	if err == nil {
		t.Error("Load() should fail for missing agent")
	}
}

func TestTOMLLoader_ConcurrentAccess(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	done := make(chan struct{})

	for i := 0; i < 10; i++ {
		go func() {
			defer func() { done <- struct{}{} }()
			_, _ = loader.Load(1)
		}()
	}

	for i := 0; i < 5; i++ {
		go func() {
			defer func() { done <- struct{}{} }()
			loader.Invalidate(1)
		}()
	}

	for i := 0; i < 15; i++ {
		<-done
	}
}
