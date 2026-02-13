package injection

import "testing"

func TestFormatAllFields(t *testing.T) {
	p := PerceptionData{
		CircadianText:   "11:42 (Du arbeitest seit 4h konzentriert)",
		BodyText:        "Hunger (85%). Dein Magen krampft.",
		EnvironmentText: "Kaffeeduft. 22.5C, stickig.",
		AcousticText:    "Lebhafte Unterhaltungen.",
		PresenceText:    "Max (konzentriert), Sophie (telefoniert).",
		ImpulseText:     "Dringendes Beduerfnis, Pause zu machen.",
	}

	got := p.Format()
	want := "CIRCADIAN: 11:42 (Du arbeitest seit 4h konzentriert)\n" +
		"KOERPER: Hunger (85%). Dein Magen krampft.\n" +
		"ENVIRONMENT: Kaffeeduft. 22.5C, stickig.\n" +
		"AKUSTIK: Lebhafte Unterhaltungen.\n" +
		"ANWESEND: Max (konzentriert), Sophie (telefoniert).\n" +
		"IMPULS: Dringendes Beduerfnis, Pause zu machen."

	if got != want {
		t.Errorf("Format() =\n%s\nwant:\n%s", got, want)
	}
}

func TestFormatSkipsEmptyFields(t *testing.T) {
	p := PerceptionData{
		CircadianText: "11:42",
		BodyText:      "Hunger (85%)",
		// EnvironmentText leer
		// AcousticText leer
		PresenceText: "Max (konzentriert)",
		// ImpulseText leer
	}

	got := p.Format()
	want := "CIRCADIAN: 11:42\n" +
		"KOERPER: Hunger (85%)\n" +
		"ANWESEND: Max (konzentriert)"

	if got != want {
		t.Errorf("Format() =\n%s\nwant:\n%s", got, want)
	}
}

func TestFormatEmpty(t *testing.T) {
	p := PerceptionData{}
	got := p.Format()
	if got != "" {
		t.Errorf("Format() = %q, want empty string", got)
	}
}

func TestFromMap(t *testing.T) {
	m := map[string]string{
		"circadian":   "08:00",
		"body":        "Energiegeladen",
		"environment": "Frisch",
		"acoustic":    "Stille",
		"presence":    "Niemand",
		"impulse":     "Kaffee holen",
	}

	p := FromMap(m)
	if p.CircadianText != "08:00" {
		t.Errorf("CircadianText = %q, want %q", p.CircadianText, "08:00")
	}
	if p.ImpulseText != "Kaffee holen" {
		t.Errorf("ImpulseText = %q, want %q", p.ImpulseText, "Kaffee holen")
	}
}

func TestFromMapMissingKeys(t *testing.T) {
	p := FromMap(map[string]string{"circadian": "10:00"})
	if p.CircadianText != "10:00" {
		t.Errorf("CircadianText = %q, want %q", p.CircadianText, "10:00")
	}
	if p.BodyText != "" {
		t.Errorf("BodyText = %q, want empty", p.BodyText)
	}
}
