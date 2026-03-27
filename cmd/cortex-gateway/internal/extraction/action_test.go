package extraction

import (
	"os"
	"testing"
)

func TestMain(m *testing.M) {
	// Initialize room aliases for all tests
	SetRoomAliases([]RoomDef{
		{ID: "kueche", Name: "Kueche / Pausenraum"},
		{ID: "empfang", Name: "Empfang"},
		{ID: "buero-dev-1", Name: "Entwicklungsbuero 1"},
		{ID: "buero-dev-2", Name: "Entwicklungsbuero 2"},
		{ID: "buero-design-1", Name: "Designbuero 1"},
		{ID: "buero-design-2", Name: "Designbuero 2"},
		{ID: "buero-ceo", Name: "Geschaeftsfuehrung"},
		{ID: "buero-sales", Name: "Vertriebsbuero"},
		{ID: "buero-pm", Name: "Projektmanagement-Buero"},
		{ID: "buero-marketing", Name: "Marketingbuero"},
		{ID: "buero-admin", Name: "Verwaltungsbuero"},
		{ID: "buero-qa", Name: "QA-Buero"},
		{ID: "buero-it", Name: "IT-Buero"},
		{ID: "meetingraum-01", Name: "Meetingraum Galileo"},
		{ID: "meetingraum-02", Name: "Meetingraum Tesla"},
		{ID: "meetingraum-03", Name: "Meetingraum Edison"},
		{ID: "toilette-eg-damen", Name: "Toilette EG Damen"},
		{ID: "toilette-eg-herren", Name: "Toilette EG Herren"},
		{ID: "treppenhaus", Name: "Treppenhaus"},
		{ID: "flur-eg", Name: "Flur Erdgeschoss"},
		{ID: "flur-og", Name: "Flur Obergeschoss"},
		{ID: "buero-betriebsrat", Name: "Betriebsratsbuero"},
		{ID: "buero-betriebspsych", Name: "Betriebspsychologie"},
		{ID: "buero-betriebsarzt", Name: "Betriebsmedizin"},
	})
	os.Exit(m.Run())
}

func TestExtract_JSONChat(t *testing.T) {
	e := New()
	actions := e.Extract(`{"action_type":"Chat","target":"Lisa","content":"Hey, wie laueft das Projekt?"}`)

	if len(actions) != 1 {
		t.Fatalf("expected 1 action, got %d", len(actions))
	}
	if actions[0].Type != "chat" {
		t.Errorf("type = %q, want %q", actions[0].Type, "chat")
	}
	if actions[0].Target != "Lisa" {
		t.Errorf("target = %q, want %q", actions[0].Target, "Lisa")
	}
	if actions[0].Content != "Hey, wie laueft das Projekt?" {
		t.Errorf("content = %q", actions[0].Content)
	}
}

func TestExtract_JSONMove(t *testing.T) {
	e := New()
	actions := e.Extract(`{"action_type":"Move","target":"Kueche","content":"Ich brauche einen Kaffee"}`)

	if len(actions) != 1 {
		t.Fatalf("expected 1 action, got %d", len(actions))
	}
	if actions[0].Type != "move" {
		t.Errorf("type = %q, want %q", actions[0].Type, "move")
	}
	if actions[0].Target != "kueche" {
		t.Errorf("target = %q, want %q", actions[0].Target, "kueche")
	}
}

func TestExtract_JSONWork(t *testing.T) {
	e := New()
	actions := e.Extract(`{"action_type":"Work","target":"Website Redesign","content":"Arbeite am Wireframe fuer die neue Homepage"}`)

	if len(actions) != 1 {
		t.Fatalf("expected 1 action, got %d", len(actions))
	}
	if actions[0].Type != "work" {
		t.Errorf("type = %q, want %q", actions[0].Type, "work")
	}
}

func TestExtract_JSONEmote(t *testing.T) {
	e := New()
	actions := e.Extract(`{"action_type":"Emote","target":"","content":"*streckt sich und gaehnt*"}`)

	if len(actions) != 1 {
		t.Fatalf("expected 1 action, got %d", len(actions))
	}
	if actions[0].Type != "emote" {
		t.Errorf("type = %q, want %q", actions[0].Type, "emote")
	}
}

func TestExtract_JSONWithSurroundingText(t *testing.T) {
	e := New()
	// LLM might wrap JSON in text
	actions := e.Extract(`Hier ist meine Antwort: {"action_type":"Chat","target":"Max","content":"Alles klar"} Ende.`)

	if len(actions) != 1 {
		t.Fatalf("expected 1 action, got %d", len(actions))
	}
	if actions[0].Type != "chat" {
		t.Errorf("type = %q, want %q", actions[0].Type, "chat")
	}
}

func TestExtract_InvalidJSONFallsBackToRegex(t *testing.T) {
	e := New()
	actions := e.Extract("Ich gehe in die Kueche, ich brauche Kaffee.")

	found := false
	for _, a := range actions {
		if a.Type == "move" {
			found = true
		}
	}
	if !found {
		t.Error("expected regex fallback to detect move")
	}
}

func TestExtract_ChatMessage(t *testing.T) {
	e := New()
	actions := e.Extract("Ok, mache ich. Kein Problem.")

	if len(actions) == 0 {
		t.Fatal("expected at least one action")
	}
	if actions[0].Type != "chat" {
		t.Errorf("type = %q, want %q", actions[0].Type, "chat")
	}
	if actions[0].Emotion != "neutral" {
		t.Errorf("emotion = %q, want %q", actions[0].Emotion, "neutral")
	}
}

func TestExtract_MoveAction(t *testing.T) {
	e := New()
	actions := e.Extract("Ich gehe in die Kueche.")

	found := false
	for _, a := range actions {
		if a.Type == "move" {
			found = true
			if a.Target != "kueche" {
				t.Errorf("target = %q, want %q", a.Target, "kueche")
			}
			break
		}
	}
	if !found {
		t.Error("expected a move action")
	}
}

func TestExtract_MoveAction_Laufe(t *testing.T) {
	e := New()
	actions := e.Extract("Ich laufe zu dem Meetingraum.")

	found := false
	for _, a := range actions {
		if a.Type == "move" {
			found = true
			if a.Target != "meetingraum-01" {
				t.Errorf("target = %q, want %q", a.Target, "meetingraum-01")
			}
			break
		}
	}
	if !found {
		t.Error("expected a move action for 'laufe zu'")
	}
}

func TestExtract_Emote(t *testing.T) {
	e := New()
	actions := e.Extract("*lacht* Das ist witzig!")

	found := false
	for _, a := range actions {
		if a.Type == "emote" {
			found = true
			if a.Content != "*lacht*" {
				t.Errorf("content = %q, want %q", a.Content, "*lacht*")
			}
			break
		}
	}
	if !found {
		t.Error("expected an emote action")
	}
}

func TestExtract_ToolUse(t *testing.T) {
	e := New()
	actions := e.Extract("Ich oeffne die Datei und schreibe den Bericht.")

	found := false
	for _, a := range actions {
		if a.Type == "tool_use" {
			found = true
			break
		}
	}
	if !found {
		t.Error("expected a tool_use action")
	}
}

func TestDetectEmotion_Happy(t *testing.T) {
	e := New()
	emotion := e.DetectEmotion("Ich freu mich total auf das Meeting!")

	if emotion != "happy" {
		t.Errorf("emotion = %q, want %q", emotion, "happy")
	}
}

func TestDetectEmotion_Frustrated(t *testing.T) {
	e := New()
	emotion := e.DetectEmotion("Ich bin so genervt von dem Bug.")

	if emotion != "frustrated" {
		t.Errorf("emotion = %q, want %q", emotion, "frustrated")
	}
}

func TestDetectEmotion_Stressed(t *testing.T) {
	e := New()
	emotion := e.DetectEmotion("Ich bin total gestresst wegen der Deadline.")

	if emotion != "stressed" {
		t.Errorf("emotion = %q, want %q", emotion, "stressed")
	}
}

func TestDetectEmotion_Tired(t *testing.T) {
	e := New()
	emotion := e.DetectEmotion("Ich bin heute so muede.")

	if emotion != "tired" {
		t.Errorf("emotion = %q, want %q", emotion, "tired")
	}
}

func TestDetectEmotion_Excited(t *testing.T) {
	e := New()
	emotion := e.DetectEmotion("Ich bin total begeistert vom neuen Design!")

	if emotion != "excited" {
		t.Errorf("emotion = %q, want %q", emotion, "excited")
	}
}

func TestDetectEmotion_Neutral(t *testing.T) {
	e := New()
	emotion := e.DetectEmotion("Ok, mache ich.")

	if emotion != "neutral" {
		t.Errorf("emotion = %q, want %q", emotion, "neutral")
	}
}

func TestDetectEmotion_CaseInsensitive(t *testing.T) {
	e := New()

	tests := []struct {
		input string
		want  string
	}{
		{"Ich bin GESTRESST!", "stressed"},
		{"FRUSTRIERT und genervt", "frustrated"},
		{"Gluecklich", "happy"},
		{"BEGEISTERT!", "excited"},
		{"Erschoepft...", "tired"},
	}

	for _, tt := range tests {
		got := e.DetectEmotion(tt.input)
		if got != tt.want {
			t.Errorf("DetectEmotion(%q) = %q, want %q", tt.input, got, tt.want)
		}
	}
}

func TestExtract_EmoteMoveRichtung(t *testing.T) {
	e := New()
	actions := e.Extract("*geht zielstrebig Richtung Kueche*")

	found := false
	for _, a := range actions {
		if a.Type == "move" {
			found = true
			if a.Target != "kueche" {
				t.Errorf("target = %q, want %q", a.Target, "kueche")
			}
			break
		}
	}
	if !found {
		t.Errorf("expected move action for '*geht Richtung Kueche*', got %+v", actions)
	}
}

func TestExtract_EmoteMoveVerlasst(t *testing.T) {
	e := New()
	actions := e.Extract("*waescht sich die Haende und verlasst die Toilette*")

	found := false
	for _, a := range actions {
		if a.Type == "move" {
			found = true
			if a.Target != "toilette-eg-herren" {
				t.Errorf("target = %q, want %q", a.Target, "toilette-eg-herren")
			}
			break
		}
	}
	if !found {
		t.Errorf("expected move action for 'verlasst', got %+v", actions)
	}
}

func TestExtract_EmoteMoveVerlaesst(t *testing.T) {
	e := New()
	actions := e.Extract("*verlaesst den Meetingraum*")

	found := false
	for _, a := range actions {
		if a.Type == "move" {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("expected move action for 'verlaesst', got %+v", actions)
	}
}

func TestExtract_EmoteMoveBetritt(t *testing.T) {
	e := New()
	actions := e.Extract("*betritt die Kueche*")

	found := false
	for _, a := range actions {
		if a.Type == "move" {
			found = true
			if a.Target != "kueche" {
				t.Errorf("target = %q, want %q", a.Target, "kueche")
			}
			break
		}
	}
	if !found {
		t.Errorf("expected move action for 'betritt', got %+v", actions)
	}
}

func TestExtract_PureEmoteNoMove(t *testing.T) {
	e := New()
	actions := e.Extract("*lacht und klopft auf den Tisch*")

	for _, a := range actions {
		if a.Type == "move" {
			t.Error("should NOT be classified as move")
		}
	}
	found := false
	for _, a := range actions {
		if a.Type == "emote" {
			found = true
		}
	}
	if !found {
		t.Error("expected emote action")
	}
}

func TestExtract_MultipleActions(t *testing.T) {
	e := New()
	actions := e.Extract("*nickt* Ich gehe in die Kueche.")

	if len(actions) < 2 {
		t.Fatalf("expected at least 2 actions, got %d", len(actions))
	}

	hasEmote := false
	hasMove := false
	for _, a := range actions {
		if a.Type == "emote" {
			hasEmote = true
		}
		if a.Type == "move" {
			hasMove = true
		}
	}
	if !hasEmote {
		t.Error("expected emote action")
	}
	if !hasMove {
		t.Error("expected move action")
	}
}
