package judge

// FatigueResult holds fatigue analysis for an agent.
type FatigueResult struct {
	AgentName      string
	FatigueScore   float64 // 0.0 = fresh, 1.0 = completely fatigued
	RepetitionRate float64
	LengthTrend    float64 // negative = shrinking responses
	Details        string
}

// FatigueDetector identifies simulation fatigue through repetitive patterns.
type FatigueDetector struct{}

func NewFatigueDetector() *FatigueDetector {
	return &FatigueDetector{}
}

// CheckFatigue analyzes messages for fatigue indicators.
func (f *FatigueDetector) CheckFatigue(agentName string, messages []string) FatigueResult {
	if len(messages) == 0 {
		return FatigueResult{
			AgentName:      agentName,
			FatigueScore:   0.0,
			RepetitionRate: 0.0,
			LengthTrend:    0.0,
			Details:        "no messages to analyze",
		}
	}

	// Calculate repetition rate: count identical message prefixes (first 20 chars)
	prefixes := make(map[string]int)
	duplicates := 0
	for _, msg := range messages {
		prefix := msg
		if len(msg) > 20 {
			prefix = msg[:20]
		}
		prefixes[prefix]++
		if prefixes[prefix] > 1 {
			duplicates++
		}
	}
	repetitionRate := float64(duplicates) / float64(len(messages))

	// Calculate length trend: compare first half vs second half
	lengthTrend := 0.0
	if len(messages) >= 2 {
		mid := len(messages) / 2
		firstHalf := messages[:mid]
		secondHalf := messages[mid:]

		firstAvg := 0.0
		for _, msg := range firstHalf {
			firstAvg += float64(len(msg))
		}
		firstAvg /= float64(len(firstHalf))

		secondAvg := 0.0
		for _, msg := range secondHalf {
			secondAvg += float64(len(msg))
		}
		secondAvg /= float64(len(secondHalf))

		if firstAvg > 0 {
			lengthTrend = (secondAvg - firstAvg) / firstAvg
		}
	}

	// Calculate fatigue score
	lengthDeclineFactor := 0.0
	if lengthTrend < 0 {
		lengthDeclineFactor = -lengthTrend
		if lengthDeclineFactor > 1.0 {
			lengthDeclineFactor = 1.0
		}
	}

	fatigueScore := (repetitionRate + lengthDeclineFactor) / 2.0
	if fatigueScore > 1.0 {
		fatigueScore = 1.0
	}

	details := "agent showing signs of fatigue"
	if fatigueScore < 0.3 {
		details = "agent appears fresh"
	} else if fatigueScore < 0.6 {
		details = "moderate fatigue detected"
	}

	return FatigueResult{
		AgentName:      agentName,
		FatigueScore:   fatigueScore,
		RepetitionRate: repetitionRate,
		LengthTrend:    lengthTrend,
		Details:        details,
	}
}
