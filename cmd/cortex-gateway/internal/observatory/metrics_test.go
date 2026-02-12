package observatory

import (
	"math"
	"testing"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/judge"
)

const floatTolerance = 0.01

func assertInRange(t *testing.T, name string, value, low, high float64) {
	t.Helper()
	if value < low || value > high {
		t.Errorf("%s = %f, want in range [%f, %f]", name, value, low, high)
	}
}

func assertApprox(t *testing.T, name string, got, want float64) {
	t.Helper()
	if math.Abs(got-want) > floatTolerance {
		t.Errorf("%s = %f, want approx %f (tolerance %f)", name, got, want, floatTolerance)
	}
}

// --- InfoPropagation Tests ---

func TestInfoPropagation(t *testing.T) {
	// 5/15 agents, 0.9 accuracy -> 0.3
	score := CalcInfoPropagation(15, 5, 0.9)
	assertInRange(t, "InfoPropagation(15,5,0.9)", score, 0.29, 0.35)
}

func TestInfoPropagationZeroAgents(t *testing.T) {
	score := CalcInfoPropagation(0, 0, 0.9)
	assertApprox(t, "InfoPropagation(0,0,0.9)", score, 0.0)
}

func TestInfoPropagationFullPropagation(t *testing.T) {
	score := CalcInfoPropagation(10, 10, 1.0)
	assertApprox(t, "InfoPropagation(10,10,1.0)", score, 1.0)
}

func TestInfoPropagationLowAccuracy(t *testing.T) {
	score := CalcInfoPropagation(10, 10, 0.0)
	assertApprox(t, "InfoPropagation(10,10,0.0)", score, 0.0)
}

// --- GroupPolarization Tests ---

func TestGroupPolarization(t *testing.T) {
	// sentiments [0.6, 0.7, 0.65, 0.8, 0.55] -> low variance, <0.3
	sentiments := []float64{0.6, 0.7, 0.65, 0.8, 0.55}
	score := CalcGroupPolarization(sentiments)
	if score >= 0.3 {
		t.Errorf("GroupPolarization = %f, want < 0.3", score)
	}
}

func TestGroupPolarizationHighVariance(t *testing.T) {
	// Extreme sentiments -> high variance
	sentiments := []float64{0.0, 1.0, 0.0, 1.0}
	score := CalcGroupPolarization(sentiments)
	if score < 0.2 {
		t.Errorf("GroupPolarization = %f, want >= 0.2 for high polarization", score)
	}
}

func TestGroupPolarizationEmpty(t *testing.T) {
	score := CalcGroupPolarization(nil)
	assertApprox(t, "GroupPolarization(nil)", score, 0.0)
}

func TestGroupPolarizationUniform(t *testing.T) {
	// All same sentiment -> variance = 0
	sentiments := []float64{0.5, 0.5, 0.5, 0.5}
	score := CalcGroupPolarization(sentiments)
	assertApprox(t, "GroupPolarization(uniform)", score, 0.0)
}

// --- PersonalityConsistency Tests ---

func TestPersonalityConsistency(t *testing.T) {
	detector := judge.NewDriftDetector()
	// Extraversion 0.5 -> expectedVerbosity ~100 chars, expectedExclamations ~1.5
	detector.RegisterProfile("Agent-01", judge.PersonalityProfile{
		Role:         "Developer",
		Extraversion: 0.5,
		Neuroticism:  0.3,
		KeyTraits:    []string{"analytical", "quiet"},
	})

	// Messages that match expected verbosity (~100 chars) and moderate exclamation use
	windows := [][]string{
		{
			"Working on the API refactoring today, focusing on the endpoint structure and the error handling layer.",
			"The database schema needs some adjustments! I will prepare the migration scripts for the new feature.",
		},
		{
			"Code review completed for the authentication module. Found a few minor issues with input validation!",
			"Deploying the staging build now. All integration tests passed with the updated configuration files.",
		},
	}

	score := CalcPersonalityConsistency(detector, "Agent-01", windows)
	// With messages matching the profile, consistency should be reasonable
	if score < 0.3 {
		t.Errorf("PersonalityConsistency = %f, want > 0.3 for profile-matching messages", score)
	}

	// Unknown agent should return high consistency (DriftDetector returns 0.0 drift)
	scoreUnknown := CalcPersonalityConsistency(detector, "Unknown-Agent", windows)
	assertApprox(t, "PersonalityConsistency(unknown)", scoreUnknown, 1.0)
}

func TestPersonalityConsistencyEmpty(t *testing.T) {
	detector := judge.NewDriftDetector()
	score := CalcPersonalityConsistency(detector, "Agent-01", nil)
	assertApprox(t, "PersonalityConsistency(nil)", score, 1.0)
}

// --- ResponseCreativity Tests ---

func TestResponseCreativity(t *testing.T) {
	// Diverse texts -> higher creativity
	diverse := []string{
		"The quantum computing revolution transforms how we approach optimization problems.",
		"Yesterday's rainfall created beautiful patterns across the garden stones.",
		"Musical theory intersects with mathematical concepts in fascinating ways.",
	}
	diverseScore := CalcResponseCreativity(diverse)

	// Repetitive texts -> lower creativity
	repetitive := []string{
		"I agree with the plan. The plan is good.",
		"I agree with the plan. The plan is good.",
		"I agree with the plan. The plan is good.",
	}
	repetitiveScore := CalcResponseCreativity(repetitive)

	if diverseScore <= repetitiveScore {
		t.Errorf("diverse creativity (%f) should be > repetitive creativity (%f)",
			diverseScore, repetitiveScore)
	}
}

func TestResponseCreativityEmpty(t *testing.T) {
	score := CalcResponseCreativity(nil)
	assertApprox(t, "ResponseCreativity(nil)", score, 0.0)
}

func TestResponseCreativitySingleWord(t *testing.T) {
	score := CalcResponseCreativity([]string{"hello"})
	// Single word: no bigrams possible, low entropy
	assertApprox(t, "ResponseCreativity(single word)", score, 0.0)
}

// --- EmotionalRange Tests ---

func TestEmotionalRange(t *testing.T) {
	// 5 of 10 possible emotions -> 0.5
	emotions := []string{"joy", "anger", "surprise", "fear", "sadness"}
	score := CalcEmotionalRange(emotions, 10)
	assertApprox(t, "EmotionalRange(5/10)", score, 0.5)
}

func TestEmotionalRangeWithDuplicates(t *testing.T) {
	// Duplicates should be deduplicated: 3 unique of 10
	emotions := []string{"joy", "joy", "anger", "anger", "surprise"}
	score := CalcEmotionalRange(emotions, 10)
	assertApprox(t, "EmotionalRange(3unique/10)", score, 0.3)
}

func TestEmotionalRangeEmpty(t *testing.T) {
	score := CalcEmotionalRange(nil, 10)
	assertApprox(t, "EmotionalRange(nil,10)", score, 0.0)
}

func TestEmotionalRangeZeroPossible(t *testing.T) {
	score := CalcEmotionalRange([]string{"joy"}, 0)
	assertApprox(t, "EmotionalRange(1,0)", score, 0.0)
}

func TestEmotionalRangeFull(t *testing.T) {
	emotions := []string{"joy", "anger", "surprise", "fear", "sadness"}
	score := CalcEmotionalRange(emotions, 5)
	assertApprox(t, "EmotionalRange(5/5)", score, 1.0)
}

// --- CommunicationScore Tests ---

func TestCommunicationScore(t *testing.T) {
	detector := judge.NewDriftDetector()
	detector.RegisterProfile("Agent-01", judge.PersonalityProfile{
		Role:         "Developer",
		Extraversion: 0.5,
		Neuroticism:  0.3,
		KeyTraits:    []string{"analytical"},
	})
	scorer := judge.NewQualityScorer(detector)

	messages := []string{
		"I reviewed the pull request for the authentication module and found 3 issues with error handling.",
		"The CI pipeline needs updating: golangci-lint v2 changed the config format significantly.",
	}

	score := CalcCommunicationScore(scorer, "Agent-01", messages)
	if score < 0.0 || score > 1.0 {
		t.Errorf("CommunicationScore = %f, want in [0, 1]", score)
	}
	// These are reasonably specific messages, should score moderately well
	if score < 0.2 {
		t.Errorf("CommunicationScore = %f, want > 0.2 for specific messages", score)
	}
}

func TestCommunicationScoreEmpty(t *testing.T) {
	detector := judge.NewDriftDetector()
	scorer := judge.NewQualityScorer(detector)
	score := CalcCommunicationScore(scorer, "Agent-01", nil)
	assertApprox(t, "CommunicationScore(nil)", score, 0.0)
}

// --- Helper function tests ---

func TestShannonEntropy(t *testing.T) {
	// All same tokens -> entropy 0
	same := []string{"a", "a", "a", "a"}
	e := shannonEntropy(same)
	assertApprox(t, "shannonEntropy(same)", e, 0.0)

	// All unique tokens -> entropy 1 (normalized)
	unique := []string{"a", "b", "c", "d"}
	e = shannonEntropy(unique)
	assertApprox(t, "shannonEntropy(unique)", e, 1.0)
}

func TestUniqueNGramRatio(t *testing.T) {
	// All unique bigrams
	tokens := []string{"a", "b", "c", "d"}
	ratio := uniqueNGramRatio(tokens, 2)
	assertApprox(t, "uniqueNGramRatio(unique)", ratio, 1.0)

	// Repeated bigrams: "a b a b" -> bigrams: "a b", "b a", "a b" -> 2/3
	tokens = []string{"a", "b", "a", "b"}
	ratio = uniqueNGramRatio(tokens, 2)
	expected := 2.0 / 3.0
	if math.Abs(ratio-expected) > floatTolerance {
		t.Errorf("uniqueNGramRatio(repeated) = %f, want approx %f", ratio, expected)
	}
}

func TestClamp01(t *testing.T) {
	tests := []struct {
		input, want float64
	}{
		{-0.5, 0.0},
		{0.0, 0.0},
		{0.5, 0.5},
		{1.0, 1.0},
		{1.5, 1.0},
	}
	for _, tc := range tests {
		got := clamp01(tc.input)
		if got != tc.want {
			t.Errorf("clamp01(%f) = %f, want %f", tc.input, got, tc.want)
		}
	}
}
