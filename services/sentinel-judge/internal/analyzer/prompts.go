// Package analyzer provides LLM-based analysis of agent behavior via Cortex Gateway.
package analyzer

// VoiceAnalysisSystemPrompt is the system prompt for voice pattern analysis.
const VoiceAnalysisSystemPrompt = `Du bist ein linguistischer Analyst fuer eine Firmen-Simulation.
Deine Aufgabe ist es, den Sprachstil eines Agenten zu analysieren.
Du darfst die vierte Wand kennen - du weisst, dass die Agenten KI-gesteuert sind.
Antworte AUSSCHLIESSLICH als valides JSON. Kein Markdown, kein Text davor oder danach.`

// VoiceAnalysisUserTemplate is the user prompt template for voice analysis.
// %s = agent name, %s = role, %d = message count, %s = messages (joined)
const VoiceAnalysisUserTemplate = `Analysiere die folgenden %d Antworten von Agent "%s" (Rolle: %s).

Identifiziere:
- Haeufige Phrasen und Redewendungen
- Satzlaenge-Tendenz (kurz/mittel/lang)
- Formalitaetsgrad (0.0 = sehr informell, 1.0 = sehr formell)

Nachrichten:
%s

Antwort als JSON:
{"phrases": ["phrase1", "phrase2"], "sentence_style": "kurz|mittel|lang", "formality": 0.X}`

// BehavioralNotesSystemPrompt is the system prompt for behavioral pattern analysis.
const BehavioralNotesSystemPrompt = `Du bist ein Verhaltensanalyst fuer eine Firmen-Simulation.
Deine Aufgabe ist es, Verhaltensmuster eines Agenten zu identifizieren.
Du darfst die vierte Wand kennen - du weisst, dass die Agenten KI-gesteuert sind.
Antworte AUSSCHLIESSLICH als valides JSON. Kein Markdown, kein Text davor oder danach.`

// BehavioralNotesUserTemplate is the user prompt template for behavioral analysis.
const BehavioralNotesUserTemplate = `Analysiere die folgenden %d Nachrichten von Agent "%s" (Rolle: %s).

Identifiziere:
- Wiederkehrende Gewohnheiten und Arbeitsroutinen
- Interaktionsmuster (proaktiv/reaktiv, kooperativ/kompetitiv)
- Entscheidungsverhalten (schnell/zoegerlich, risikofreudig/vorsichtig)
- Auffaellige Verhaltensaenderungen im Verlauf

Nachrichten:
%s

Antwort als JSON:
{"habits": ["habit1", "habit2"], "interaction_style": "proaktiv|reaktiv|gemischt", "decision_style": "schnell|zoegerlich|ausgewogen", "anomalies": ["anomaly1"]}`

// NarrativeArcSystemPrompt is the system prompt for narrative arc analysis.
const NarrativeArcSystemPrompt = `Du bist ein Narrativ-Analyst fuer eine Firmen-Simulation.
Deine Aufgabe ist es, den narrativen Verlauf einer Schicht zu analysieren.
Du darfst die vierte Wand kennen - du weisst, dass die Agenten KI-gesteuert sind.
Antworte AUSSCHLIESSLICH als valides JSON. Kein Markdown, kein Text davor oder danach.`

// NarrativeArcUserTemplate is the user prompt template for narrative arc analysis.
const NarrativeArcUserTemplate = `Analysiere den narrativen Verlauf der folgenden %d Nachrichten von Agent "%s" (Rolle: %s) waehrend einer Schicht.

Identifiziere:
- Die Gesamtstimmung der Schicht (positiv/neutral/negativ)
- Wichtige Wendepunkte oder Stimmungswechsel
- Das zentrale Thema der Schicht
- Einen kurzen narrativen Bogen (Anfang, Mitte, Ende der Schicht)

Nachrichten:
%s

Antwort als JSON:
{"mood": "positiv|neutral|negativ", "turning_points": ["punkt1"], "theme": "Thema der Schicht", "arc_summary": "Kurze Zusammenfassung des narrativen Bogens"}`

// RelationshipDynamicsSystemPrompt is the system prompt for relationship analysis.
const RelationshipDynamicsSystemPrompt = `Du bist ein Beziehungsdynamik-Analyst fuer eine Firmen-Simulation.
Deine Aufgabe ist es, die sozialen Beziehungen eines Agenten zu analysieren.
Du darfst die vierte Wand kennen - du weisst, dass die Agenten KI-gesteuert sind.
Antworte AUSSCHLIESSLICH als valides JSON. Kein Markdown, kein Text davor oder danach.`

// RelationshipDynamicsUserTemplate is the user prompt template for relationship analysis.
const RelationshipDynamicsUserTemplate = `Analysiere die sozialen Beziehungen in den folgenden %d Nachrichten von Agent "%s" (Rolle: %s).

Identifiziere:
- Erwaehnte Kollegen und die Beziehungsqualitaet (positiv/neutral/negativ)
- Kollaborationsmuster (mit wem wird zusammengearbeitet?)
- Konflikte oder Spannungen
- Soziale Rolle im Team (Fuehrung/Unterstuetzung/Einzelgaenger)

Nachrichten:
%s

Antwort als JSON:
{"relationships": [{"colleague": "Name", "quality": "positiv|neutral|negativ"}], "collaboration_partners": ["Name1"], "conflicts": ["Beschreibung"], "team_role": "fuehrend|unterstuetzend|einzelgaenger|vermittelnd"}`
