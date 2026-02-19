package judge

import "testing"

func TestDriftDetector_UnknownAgent(t *testing.T) {
	detector := NewDriftDetector()

	result := detector.CheckDrift("unknown-agent", []string{"hello world"})

	if result.DriftScore != 0.0 {
		t.Errorf("expected DriftScore 0.0 for unknown agent, got %.2f", result.DriftScore)
	}
	if result.Severity != "none" {
		t.Errorf("expected severity 'none' for unknown agent, got %s", result.Severity)
	}
	if result.Details != "unknown agent" {
		t.Errorf("expected details 'unknown agent', got %s", result.Details)
	}
}

func TestDriftDetector_IntrovertBecomesExtrovert(t *testing.T) {
	detector := NewDriftDetector()
	detector.RegisterProfile("introvert-agent", PersonalityProfile{
		Role:         "quiet analyst",
		Extraversion: 0.2,
		Neuroticism:  0.3,
		KeyTraits:    []string{"calm", "analytical"},
	})

	messages := []string{
		"This is amazing!!!",
		"Wow!! This is great!!",
		"I'm so excited!!!",
	}

	result := detector.CheckDrift("introvert-agent", messages)

	if result.DriftScore < 0.5 {
		t.Errorf("expected high drift score for introvert with exclamations, got %.2f", result.DriftScore)
	}
	if result.Severity == "none" {
		t.Errorf("expected severity other than 'none', got %s", result.Severity)
	}
}

func TestDriftDetector_ConsistentBehavior(t *testing.T) {
	detector := NewDriftDetector()
	detector.RegisterProfile("extrovert-agent", PersonalityProfile{
		Role:         "energetic salesperson",
		Extraversion: 0.8,
		Neuroticism:  0.4,
		KeyTraits:    []string{"energetic", "outgoing"},
	})

	messages := []string{
		"Hey there! This is great! Really excited!!",
		"I'm super excited about this! Can't wait!",
		"Let's do this! Amazing opportunity! This is fantastic!",
	}

	result := detector.CheckDrift("extrovert-agent", messages)

	if result.DriftScore > 0.6 {
		t.Errorf("expected reasonably low drift score for consistent behavior, got %.2f", result.DriftScore)
	}
	if result.Severity == "critical" {
		t.Errorf("expected severity not critical for consistent behavior, got %s", result.Severity)
	}
	// Note: Some drift is expected due to heuristic nature of detection
}

func TestDriftDetector_EmptyMessages(t *testing.T) {
	detector := NewDriftDetector()
	detector.RegisterProfile("test-agent", PersonalityProfile{
		Role:         "tester",
		Extraversion: 0.5,
		Neuroticism:  0.5,
		KeyTraits:    []string{"balanced"},
	})

	result := detector.CheckDrift("test-agent", []string{})

	if result.DriftScore != 0.0 {
		t.Errorf("expected DriftScore 0.0 for empty messages, got %.2f", result.DriftScore)
	}
	if result.Severity != "none" {
		t.Errorf("expected severity 'none' for empty messages, got %s", result.Severity)
	}
}
