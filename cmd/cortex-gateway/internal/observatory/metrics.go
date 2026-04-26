package observatory

import (
	"math"
	"strings"

	"github.com/silentspike/project-sentinel/pkg/sentinel-go/judge"
)

// CalcInfoPropagation calculates how well information spreads through the agent network.
// Formula: (agentsReached / totalAgents) * accuracy
// Returns 0.0 if totalAgents is 0.
func CalcInfoPropagation(totalAgents, agentsReached int, accuracy float64) float64 {
	if totalAgents <= 0 {
		return 0.0
	}
	ratio := float64(agentsReached) / float64(totalAgents)
	result := ratio * accuracy
	return clamp01(result)
}

// CalcGroupPolarization calculates variance of sentiment values as a polarization indicator.
// Formula: Σ(xi - mean)² / N (population variance)
// Low variance = consensus, high variance = polarization.
// Returns 0.0 for empty input.
func CalcGroupPolarization(sentiments []float64) float64 {
	n := len(sentiments)
	if n == 0 {
		return 0.0
	}

	// Calculate mean
	sum := 0.0
	for _, s := range sentiments {
		sum += s
	}
	mean := sum / float64(n)

	// Calculate variance
	variance := 0.0
	for _, s := range sentiments {
		diff := s - mean
		variance += diff * diff
	}
	variance /= float64(n)

	return variance
}

// CalcCommunicationScore integrates with judge.QualityScorer to evaluate message quality.
// Returns the average QualityScorer score normalized to 0-1 (score / 5.0).
// Returns 0.0 for empty messages.
func CalcCommunicationScore(scorer *judge.QualityScorer, agentName string, messages []string) float64 {
	if len(messages) == 0 {
		return 0.0
	}

	totalScore := 0.0
	for i, msg := range messages {
		// Use previous messages as recent history for context
		var history []string
		if i > 0 {
			history = messages[:i]
		}
		result := scorer.ScoreMessage(agentName, msg, history)
		totalScore += float64(result.Score)
	}

	avgScore := totalScore / float64(len(messages))
	// Normalize to 0-1 range (QualityScorer returns 1-5)
	return avgScore / 5.0
}

// CalcPersonalityConsistency uses the DriftDetector to measure how consistent an agent stays.
// Formula: 1.0 - average DriftScore across all message windows.
// Returns 1.0 (perfect consistency) for empty windows.
func CalcPersonalityConsistency(detector *judge.DriftDetector, agentName string, messageWindows [][]string) float64 {
	if len(messageWindows) == 0 {
		return 1.0
	}

	totalDrift := 0.0
	for _, window := range messageWindows {
		result := detector.CheckDrift(agentName, window)
		totalDrift += result.DriftScore
	}

	avgDrift := totalDrift / float64(len(messageWindows))
	return clamp01(1.0 - avgDrift)
}

// CalcResponseCreativity measures lexical diversity through entropy and n-gram uniqueness.
// Formula: 0.5 * shannonEntropy(tokens) + 0.5 * uniqueBigramRatio
// Returns 0.0 for empty input.
func CalcResponseCreativity(responses []string) float64 {
	if len(responses) == 0 {
		return 0.0
	}

	// Collect all tokens from all responses
	var allTokens []string
	for _, resp := range responses {
		tokens := tokenize(resp)
		allTokens = append(allTokens, tokens...)
	}

	if len(allTokens) == 0 {
		return 0.0
	}

	// Shannon entropy of token distribution (normalized)
	entropy := shannonEntropy(allTokens)

	// Unique bigram ratio
	bigramRatio := uniqueNGramRatio(allTokens, 2)

	return clamp01(0.5*entropy + 0.5*bigramRatio)
}

// CalcEmotionalRange measures the breadth of detected emotions.
// Formula: len(unique(detectedEmotions)) / possibleEmotions
// Returns 0.0 if possibleEmotions is 0.
func CalcEmotionalRange(detectedEmotions []string, possibleEmotions int) float64 {
	if possibleEmotions <= 0 {
		return 0.0
	}

	unique := make(map[string]struct{})
	for _, e := range detectedEmotions {
		unique[e] = struct{}{}
	}

	return clamp01(float64(len(unique)) / float64(possibleEmotions))
}

// --- Helper functions ---

// clamp01 restricts a value to the [0, 1] range.
func clamp01(v float64) float64 {
	if v < 0.0 {
		return 0.0
	}
	if v > 1.0 {
		return 1.0
	}
	return v
}

// tokenize splits text into lowercase tokens.
func tokenize(text string) []string {
	words := strings.Fields(strings.ToLower(text))
	return words
}

// shannonEntropy calculates normalized Shannon entropy of a token distribution.
// Returns a value in [0, 1] where 1 means maximum diversity.
func shannonEntropy(tokens []string) float64 {
	if len(tokens) == 0 {
		return 0.0
	}

	freq := make(map[string]int)
	for _, t := range tokens {
		freq[t]++
	}

	n := float64(len(tokens))
	entropy := 0.0
	for _, count := range freq {
		p := float64(count) / n
		if p > 0 {
			entropy -= p * math.Log2(p)
		}
	}

	// Normalize by max possible entropy (log2 of unique token count)
	uniqueCount := float64(len(freq))
	if uniqueCount <= 1 {
		return 0.0
	}
	maxEntropy := math.Log2(uniqueCount)
	if maxEntropy == 0 {
		return 0.0
	}

	return entropy / maxEntropy
}

// uniqueNGramRatio calculates the ratio of unique n-grams to total n-grams.
func uniqueNGramRatio(tokens []string, n int) float64 {
	if len(tokens) < n {
		return 0.0
	}

	totalNGrams := len(tokens) - n + 1
	unique := make(map[string]struct{})
	for i := 0; i <= len(tokens)-n; i++ {
		ngram := strings.Join(tokens[i:i+n], " ")
		unique[ngram] = struct{}{}
	}

	return float64(len(unique)) / float64(totalNGrams)
}
