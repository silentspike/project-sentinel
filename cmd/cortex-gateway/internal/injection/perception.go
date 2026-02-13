package injection

import "strings"

// PerceptionData haelt die 6 Wahrnehmungsfelder eines Agenten.
// Die Felder entsprechen den Rust-seitigen Perception-Struct-Feldern
// aus sentinel-common/src/types.rs.
type PerceptionData struct {
	CircadianText   string
	BodyText        string
	EnvironmentText string
	AcousticText    string
	PresenceText    string
	ImpulseText     string
}

// perceptionField koppelt ein Label mit einem Struct-Feld.
type perceptionField struct {
	label string
	value *string
}

// Format erzeugt den Perception-Block fuer den Compiler.
// Der Compiler wrapped den Output in [SYSTEM_INJECTION]...[/SYSTEM_INJECTION].
// Leere Felder werden uebersprungen.
func (p *PerceptionData) Format() string {
	fields := []perceptionField{
		{"CIRCADIAN", &p.CircadianText},
		{"KOERPER", &p.BodyText},
		{"ENVIRONMENT", &p.EnvironmentText},
		{"AKUSTIK", &p.AcousticText},
		{"ANWESEND", &p.PresenceText},
		{"IMPULS", &p.ImpulseText},
	}

	var b strings.Builder
	for _, f := range fields {
		if *f.value == "" {
			continue
		}
		if b.Len() > 0 {
			b.WriteByte('\n')
		}
		b.WriteString(f.label)
		b.WriteString(": ")
		b.WriteString(*f.value)
	}
	return b.String()
}

// FromMap erstellt PerceptionData aus einer Metadata-Map.
// Erwartet Keys: "circadian", "body", "environment", "acoustic", "presence", "impulse"
// oder alternativ den einzelnen Key "perception" mit dem gesamten Block.
func FromMap(m map[string]string) PerceptionData {
	return PerceptionData{
		CircadianText:   m["circadian"],
		BodyText:        m["body"],
		EnvironmentText: m["environment"],
		AcousticText:    m["acoustic"],
		PresenceText:    m["presence"],
		ImpulseText:     m["impulse"],
	}
}
