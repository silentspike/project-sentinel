package judge

import (
	"fmt"
	"strings"
	"testing"
)

// BenchmarkDriftDetector measures single-agent drift check latency.
// Target: <5ms/event (heuristic pipeline budget).
func BenchmarkDriftDetector(b *testing.B) {
	d := NewDriftDetector()
	d.RegisterProfile("AGENT-07", PersonalityProfile{
		Role:         "designer",
		Extraversion: 0.7,
		Neuroticism:  0.3,
		KeyTraits:    []string{"kreativ", "enthusiastisch"},
	})

	messages := []string{
		"Das Design ist super geworden! Ich freue mich total!",
		"Hier ist der neue Entwurf fuer die Landing Page.",
		"Die Farben passen perfekt zum Corporate Design!",
		"Ich habe noch eine Idee fuer das Header-Image.",
		"Schaut euch mal diese Typografie an!",
	}

	b.ResetTimer()
	for range b.N {
		_ = d.CheckDrift("AGENT-07", messages)
	}
}

// BenchmarkDriftDetector_15Agents simulates a full shift check (15 agents).
func BenchmarkDriftDetector_15Agents(b *testing.B) {
	d := NewDriftDetector()
	for i := 1; i <= 15; i++ {
		d.RegisterProfile(fmt.Sprintf("AGENT-%02d", i), PersonalityProfile{
			Role:         "developer",
			Extraversion: float64(i) / 15.0,
			Neuroticism:  0.3,
		})
	}

	messages := []string{
		"Ich arbeite am Feature.",
		"Der Code ist fertig.",
		"Hier sind die Aenderungen.",
	}

	b.ResetTimer()
	for range b.N {
		for i := 1; i <= 15; i++ {
			_ = d.CheckDrift(fmt.Sprintf("AGENT-%02d", i), messages)
		}
	}
}

// BenchmarkQualityScorer measures single message quality scoring.
func BenchmarkQualityScorer(b *testing.B) {
	d := NewDriftDetector()
	d.RegisterProfile("AGENT-03", PersonalityProfile{
		Role:         "developer",
		Extraversion: 0.4,
		Neuroticism:  0.5,
	})
	q := NewQualityScorer(d)

	message := "Ich habe den Bug in der Authentication-Klasse gefunden. Das Problem war ein Race Condition im Session-Handler. Fix ist im Branch feature/auth-fix."
	history := []string{
		"Heute arbeite ich an dem Login-Feature.",
		"Die Tests laufen durch.",
	}

	b.ResetTimer()
	for range b.N {
		_ = q.ScoreMessage("AGENT-03", message, history)
	}
}

// BenchmarkFatigueDetector measures fatigue detection latency.
func BenchmarkFatigueDetector(b *testing.B) {
	f := NewFatigueDetector()

	messages := make([]string, 20)
	for i := range messages {
		messages[i] = fmt.Sprintf("Nachricht Nummer %d mit etwas Inhalt zum Analysieren.", i+1)
	}

	b.ResetTimer()
	for range b.N {
		_ = f.CheckFatigue("AGENT-01", messages)
	}
}

// BenchmarkFatigueDetector_HighRepetition benchmarks worst-case repetitive input.
func BenchmarkFatigueDetector_HighRepetition(b *testing.B) {
	f := NewFatigueDetector()

	messages := make([]string, 50)
	for i := range messages {
		messages[i] = "Ja, ich arbeite daran. " + strings.Repeat("x", i)
	}

	b.ResetTimer()
	for range b.N {
		_ = f.CheckFatigue("AGENT-01", messages)
	}
}

// BenchmarkSwapTrigger measures swap decision latency.
func BenchmarkSwapTrigger(b *testing.B) {
	s := NewSwapTrigger(5, 2.0)

	b.ResetTimer()
	for range b.N {
		s.RecordScore("AGENT-01", 4)
		s.RecordScore("AGENT-01", 2)
		s.RecordScore("AGENT-01", 1)
		_ = s.ShouldSwap("AGENT-01")
		s.Reset("AGENT-01")
	}
}

// BenchmarkHeuristicPipeline measures the full 4-algorithm pipeline per event.
// This is the critical path for streaming NATS events. Target: <5ms.
func BenchmarkHeuristicPipeline(b *testing.B) {
	drift := NewDriftDetector()
	drift.RegisterProfile("AGENT-07", PersonalityProfile{
		Role:         "designer",
		Extraversion: 0.7,
		Neuroticism:  0.3,
	})
	quality := NewQualityScorer(drift)
	fatigue := NewFatigueDetector()
	swap := NewSwapTrigger(5, 2.0)

	messages := []string{
		"Das Mockup fuer die Startseite ist fertig.",
		"Ich arbeite jetzt am responsiven Layout.",
		"Die Icons brauchen noch eine Ueberarbeitung.",
		"Hier ist der aktuelle Stand vom Prototyp.",
		"Feedback von Michael zum Header eingebaut.",
	}

	b.ResetTimer()
	for range b.N {
		// Full pipeline: Drift → Quality → Fatigue → Swap
		dResult := drift.CheckDrift("AGENT-07", messages)
		latest := messages[len(messages)-1]
		history := messages[:len(messages)-1]
		qResult := quality.ScoreMessage("AGENT-07", latest, history)
		_ = fatigue.CheckFatigue("AGENT-07", messages)
		swap.RecordScore("AGENT-07", qResult.Score)
		_ = swap.ShouldSwap("AGENT-07")

		// Prevent compiler optimization
		if dResult.DriftScore < 0 {
			b.Fatal("impossible")
		}
	}
}
