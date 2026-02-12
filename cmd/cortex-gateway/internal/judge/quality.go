package judge

import (
	"strings"
	"unicode"
)

// QualityResult holds quality assessment for an agent message.
type QualityResult struct {
	AgentName string
	Score     int // 1-5
	Factors   QualityFactors
	Details   string
}

type QualityFactors struct {
	LengthScore      int // 1-5: too short=1, appropriate=5
	SpecificityScore int // 1-5: generic=1, specific=5
	ConsistencyScore int // 1-5: out of character=1, in character=5
}

// QualityScorer evaluates agent responses for realism.
type QualityScorer struct {
	Detector *DriftDetector // Reuses drift detection for consistency
}

func NewQualityScorer(detector *DriftDetector) *QualityScorer {
	return &QualityScorer{
		Detector: detector,
	}
}

// ScoreMessage evaluates a single message for quality.
func (q *QualityScorer) ScoreMessage(agentName string, message string, recentHistory []string) QualityResult {
	factors := QualityFactors{}

	// Length score
	msgLen := len(message)
	if msgLen < 10 {
		factors.LengthScore = 1
	} else if msgLen < 30 {
		factors.LengthScore = 2
	} else if msgLen < 100 {
		factors.LengthScore = 3
	} else if msgLen < 300 {
		factors.LengthScore = 4
	} else {
		factors.LengthScore = 5
	}

	// Specificity score: count concrete words (names, numbers, technical terms)
	specificityCount := countSpecificWords(message)
	if specificityCount == 0 {
		factors.SpecificityScore = 1
	} else if specificityCount <= 2 {
		factors.SpecificityScore = 2
	} else if specificityCount <= 4 {
		factors.SpecificityScore = 3
	} else if specificityCount <= 7 {
		factors.SpecificityScore = 4
	} else {
		factors.SpecificityScore = 5
	}

	// Consistency score: use DriftDetector
	historyWithCurrent := append(recentHistory, message)
	driftResult := q.Detector.CheckDrift(agentName, historyWithCurrent)
	if driftResult.DriftScore < 0.3 {
		factors.ConsistencyScore = 5
	} else if driftResult.DriftScore < 0.5 {
		factors.ConsistencyScore = 4
	} else if driftResult.DriftScore < 0.7 {
		factors.ConsistencyScore = 3
	} else if driftResult.DriftScore < 0.85 {
		factors.ConsistencyScore = 2
	} else {
		factors.ConsistencyScore = 1
	}

	// Calculate overall score (rounded average)
	totalScore := factors.LengthScore + factors.SpecificityScore + factors.ConsistencyScore
	avgScore := float64(totalScore) / 3.0
	overallScore := int(avgScore + 0.5) // round to nearest int

	var details string
	if overallScore >= 4 {
		details = "high quality response"
	} else if overallScore <= 2 {
		details = "low quality response, needs improvement"
	} else {
		details = "moderate quality response"
	}

	return QualityResult{
		AgentName: agentName,
		Score:     overallScore,
		Factors:   factors,
		Details:   details,
	}
}

// countSpecificWords counts words that start with uppercase (not at sentence start) and numbers
func countSpecificWords(text string) int {
	count := 0
	words := strings.Fields(text)

	for i, word := range words {
		// Skip empty words
		if len(word) == 0 {
			continue
		}

		// Count numbers
		hasDigit := false
		for _, r := range word {
			if unicode.IsDigit(r) {
				hasDigit = true
				break
			}
		}
		if hasDigit {
			count++
			continue
		}

		// Count words starting with uppercase (but not at start of sentence)
		if i > 0 {
			firstRune := []rune(word)[0]
			if unicode.IsUpper(firstRune) {
				count++
			}
		}
	}

	return count
}
