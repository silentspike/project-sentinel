package extraction

import (
	"testing"
)

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
			if a.Target != "Kueche" {
				t.Errorf("target = %q, want %q", a.Target, "Kueche")
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
			if a.Target != "Meetingraum" {
				t.Errorf("target = %q, want %q", a.Target, "Meetingraum")
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
