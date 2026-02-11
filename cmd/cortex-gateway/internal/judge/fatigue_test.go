package judge

import "testing"

func TestFatigueDetector_HighRepetition(t *testing.T) {
	detector := NewFatigueDetector()

	messages := []string{
		"Hello, how can I help you?",
		"Hello, how can I help you?",
		"Hello, how can I help you?",
		"Hello, how can I help you?",
		"Hello, how can I help you?",
		"Hello, how can I help you?",
	}

	result := detector.CheckFatigue("test-agent", messages)

	if result.FatigueScore < 0.4 {
		t.Errorf("expected high fatigue score for repetitive messages, got %.2f", result.FatigueScore)
	}
	if result.RepetitionRate < 0.4 {
		t.Errorf("expected high repetition rate, got %.2f", result.RepetitionRate)
	}
	// Note: Repetition detection is conservative to avoid false positives
}

func TestFatigueDetector_FreshMessages(t *testing.T) {
	detector := NewFatigueDetector()

	messages := []string{
		"Let me analyze the data for you.",
		"I found three different approaches we could take.",
		"Based on the requirements, option B seems optimal.",
		"Would you like me to explain the trade-offs?",
	}

	result := detector.CheckFatigue("test-agent", messages)

	if result.FatigueScore > 0.4 {
		t.Errorf("expected low fatigue score for varied messages, got %.2f", result.FatigueScore)
	}
	if result.RepetitionRate > 0.3 {
		t.Errorf("expected low repetition rate, got %.2f", result.RepetitionRate)
	}
}

func TestFatigueDetector_EmptyMessages(t *testing.T) {
	detector := NewFatigueDetector()

	result := detector.CheckFatigue("test-agent", []string{})

	if result.FatigueScore != 0.0 {
		t.Errorf("expected fatigue score 0.0 for empty messages, got %.2f", result.FatigueScore)
	}
	if result.RepetitionRate != 0.0 {
		t.Errorf("expected repetition rate 0.0, got %.2f", result.RepetitionRate)
	}
	if result.Details != "no messages to analyze" {
		t.Errorf("expected details 'no messages to analyze', got %s", result.Details)
	}
}

func TestFatigueDetector_DecreasingLength(t *testing.T) {
	detector := NewFatigueDetector()

	messages := []string{
		"This is a very long and detailed response with lots of information and context.",
		"Here is another comprehensive answer with multiple points and examples.",
		"Shorter answer here.",
		"Brief reply.",
		"Yes.",
	}

	result := detector.CheckFatigue("test-agent", messages)

	if result.LengthTrend >= 0 {
		t.Errorf("expected negative length trend (shrinking responses), got %.2f", result.LengthTrend)
	}
	if result.FatigueScore < 0.3 {
		t.Errorf("expected noticeable fatigue score for shrinking responses, got %.2f", result.FatigueScore)
	}
}
