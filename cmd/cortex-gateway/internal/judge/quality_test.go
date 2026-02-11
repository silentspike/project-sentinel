package judge

import "testing"

func TestQualityScorer_HighQuality(t *testing.T) {
	detector := NewDriftDetector()
	detector.RegisterProfile("developer-agent", PersonalityProfile{
		Role:         "senior developer",
		Extraversion: 0.5,
		Neuroticism:  0.3,
		KeyTraits:    []string{"technical", "precise"},
	})

	scorer := NewQualityScorer(detector)

	message := "I've analyzed the PostgreSQL performance bottleneck in the UserService module. The issue stems from the N+1 query pattern in line 247. We should implement eager loading using JOIN operations, which will reduce database round-trips from 1000+ to a single query."

	recentHistory := []string{
		"Let me review the codebase for performance issues.",
		"I found several optimization opportunities in the database layer.",
	}

	result := scorer.ScoreMessage("developer-agent", message, recentHistory)

	if result.Score < 4 {
		t.Errorf("expected high quality score (4-5) for detailed technical message, got %d", result.Score)
	}
	if result.Factors.LengthScore < 4 {
		t.Errorf("expected high length score, got %d", result.Factors.LengthScore)
	}
	if result.Factors.SpecificityScore < 3 {
		t.Errorf("expected high specificity score for technical terms, got %d", result.Factors.SpecificityScore)
	}
}

func TestQualityScorer_LowQuality(t *testing.T) {
	detector := NewDriftDetector()
	detector.RegisterProfile("agent", PersonalityProfile{
		Role:         "generic",
		Extraversion: 0.5,
		Neuroticism:  0.5,
		KeyTraits:    []string{},
	})

	scorer := NewQualityScorer(detector)

	message := "ok"

	result := scorer.ScoreMessage("agent", message, []string{})

	if result.Score > 2 {
		t.Errorf("expected low quality score (1-2) for short generic message, got %d", result.Score)
	}
	if result.Factors.LengthScore != 1 {
		t.Errorf("expected length score 1 for very short message, got %d", result.Factors.LengthScore)
	}
}

func TestQualityScorer_EmptyMessage(t *testing.T) {
	detector := NewDriftDetector()
	detector.RegisterProfile("agent", PersonalityProfile{
		Role:         "test",
		Extraversion: 0.5,
		Neuroticism:  0.5,
		KeyTraits:    []string{},
	})

	scorer := NewQualityScorer(detector)

	result := scorer.ScoreMessage("agent", "", []string{})

	if result.Score > 2 {
		t.Errorf("expected low quality score for empty message, got %d", result.Score)
	}
	if result.Factors.LengthScore != 1 {
		t.Errorf("expected length score 1 for empty message, got %d", result.Factors.LengthScore)
	}
}

func TestQualityScorer_ModerateQuality(t *testing.T) {
	detector := NewDriftDetector()
	detector.RegisterProfile("agent", PersonalityProfile{
		Role:         "assistant",
		Extraversion: 0.6,
		Neuroticism:  0.4,
		KeyTraits:    []string{"helpful"},
	})

	scorer := NewQualityScorer(detector)

	message := "I can help you with that task. Let me know what you need."

	recentHistory := []string{
		"Hello, how can I assist you today?",
	}

	result := scorer.ScoreMessage("agent", message, recentHistory)

	if result.Score < 2 || result.Score > 4 {
		t.Errorf("expected moderate quality score (2-4), got %d", result.Score)
	}
}
