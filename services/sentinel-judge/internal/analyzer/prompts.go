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
