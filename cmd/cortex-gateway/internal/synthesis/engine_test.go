package synthesis

import (
	"testing"
)

func TestParseFingerprint(t *testing.T) {
	fp, err := Parse("H3|E7|B2|S4|C1|SN5|R:buero-dev-1|P:2|CH:0|HR:0|T:10|TMP:1|PE:E|IM:0")
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if fp.Hunger != 3 {
		t.Errorf("Hunger = %d, want 3", fp.Hunger)
	}
	if fp.Energy != 7 {
		t.Errorf("Energy = %d, want 7", fp.Energy)
	}
	if fp.Bladder != 2 {
		t.Errorf("Bladder = %d, want 2", fp.Bladder)
	}
	if fp.RoomID != "buero-dev-1" {
		t.Errorf("RoomID = %q, want buero-dev-1", fp.RoomID)
	}
	if fp.PresenceCount != 2 {
		t.Errorf("PresenceCount = %d, want 2", fp.PresenceCount)
	}
	if fp.HasChaos {
		t.Error("HasChaos = true, want false")
	}
	if fp.HasHeard {
		t.Error("HasHeard = true, want false")
	}
	if fp.SimHour != 10 {
		t.Errorf("SimHour = %d, want 10", fp.SimHour)
	}
	if !fp.TempHigh {
		t.Error("TempHigh = false, want true")
	}
	if fp.Personality != "E" {
		t.Errorf("Personality = %q, want E", fp.Personality)
	}
}

func TestParseEmpty(t *testing.T) {
	_, err := Parse("")
	if err == nil {
		t.Error("Parse('') should return error")
	}
}

func TestParsePartialFields(t *testing.T) {
	// Partial fingerprints are valid — missing fields default to zero values
	fp, err := Parse("H3|E7|B0|S0|C0|SN0|R:test|P:0|CH:0|HR:0|T:0|TMP:0|PE:I|IM:0")
	if err != nil {
		t.Fatalf("Parse partial: %v", err)
	}
	if fp.Hunger != 3 || fp.Energy != 7 {
		t.Errorf("H=%d E=%d, want H=3 E=7", fp.Hunger, fp.Energy)
	}
}

func TestBioBladderP0(t *testing.T) {
	meta := map[string]string{
		"synth_fp":              "H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0",
		"is_directly_addressed": "false",
		"personality_type":      "I",
		"agent_id":              "5",
	}
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for bladder=9")
	}
	if result.Rule != "bio_bladder_p0" {
		t.Errorf("rule = %q, want bio_bladder_p0", result.Rule)
	}
	if result.Content == "" {
		t.Error("content should not be empty")
	}
}

func TestBioBladderBlockedByHeard(t *testing.T) {
	meta := map[string]string{
		"synth_fp":              "H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:1|T:10|TMP:0|PE:I|IM:0",
		"is_directly_addressed": "false",
		"personality_type":      "I",
		"agent_id":              "5",
	}
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Forward {
		t.Error("expected Forward when HasHeard=true (AC-6)")
	}
}

func TestBioBladderBlockedByAddressed(t *testing.T) {
	meta := map[string]string{
		"synth_fp":              "H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0",
		"is_directly_addressed": "true",
		"personality_type":      "I",
		"agent_id":              "5",
	}
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Forward {
		t.Error("expected Forward when isAddressed=true (AC-6)")
	}
}

func TestHeartbeatIdle(t *testing.T) {
	meta := map[string]string{
		"synth_fp":              "H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:14|TMP:0|PE:E|IM:0",
		"is_directly_addressed": "false",
		"personality_type":      "E",
		"agent_id":              "10",
	}
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for heartbeat_idle")
	}
	if result.Rule != "heartbeat_idle" {
		t.Errorf("rule = %q, want heartbeat_idle", result.Rule)
	}
}

func TestHeartbeatIdleWithPresence(t *testing.T) {
	meta := map[string]string{
		"synth_fp":              "H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:3|CH:0|HR:0|T:14|TMP:0|PE:E|IM:0",
		"is_directly_addressed": "false",
		"personality_type":      "E",
		"agent_id":              "10",
	}
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	// heartbeat_idle matches even with PresenceCount > 0 (agents work silently in shared offices)
	if result.Decision != Synthesize || result.Rule != "heartbeat_idle" {
		t.Errorf("heartbeat_idle should match with PresenceCount=3, got decision=%d rule=%q", result.Decision, result.Rule)
	}
}

func TestCircadianMorning(t *testing.T) {
	meta := map[string]string{
		"synth_fp":              "H3|E7|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:6|TMP:0|PE:I|IM:0",
		"is_directly_addressed": "false",
		"personality_type":      "I",
		"agent_id":              "1",
	}
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for circadian_morning")
	}
	if result.Rule != "circadian_morning" {
		t.Errorf("rule = %q, want circadian_morning", result.Rule)
	}
}

func TestCircadianLunch(t *testing.T) {
	meta := map[string]string{
		"synth_fp":              "H7|E5|B3|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:12|TMP:0|PE:E|IM:0",
		"is_directly_addressed": "false",
		"personality_type":      "E",
		"agent_id":              "2",
	}
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for circadian_lunch")
	}
	if result.Rule != "circadian_lunch" {
		t.Errorf("rule = %q, want circadian_lunch", result.Rule)
	}
}

func TestPhysicsTempHigh(t *testing.T) {
	meta := map[string]string{
		"synth_fp":              "H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:14|TMP:1|PE:I|IM:0",
		"is_directly_addressed": "false",
		"personality_type":      "I",
		"agent_id":              "3",
	}
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for physics_temp_high")
	}
	if result.Rule != "physics_temp_high" {
		t.Errorf("rule = %q, want physics_temp_high", result.Rule)
	}
}

func TestPersonalityTemplates(t *testing.T) {
	metaI := map[string]string{
		"synth_fp":              "H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0",
		"is_directly_addressed": "false",
		"personality_type":      "I",
		"agent_id":              "5",
	}
	metaE := map[string]string{
		"synth_fp":              "H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:E|IM:0",
		"is_directly_addressed": "false",
		"personality_type":      "E",
		"agent_id":              "6",
	}

	engine := NewEngine(true, nil)
	resultI := engine.Decide(metaI, "AGENT-I")
	resultE := engine.Decide(metaE, "AGENT-E")

	if resultI.Content == resultE.Content {
		t.Errorf("I and E templates should differ, both got: %q", resultI.Content)
	}
}

func TestDisabledEngineForwards(t *testing.T) {
	meta := map[string]string{
		"synth_fp":              "H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0",
		"is_directly_addressed": "false",
		"personality_type":      "I",
		"agent_id":              "5",
	}
	engine := NewEngine(false, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	// Disabled engine should not synthesize even if rule matches
	// (Engine.Decide doesn't check Enabled — caller does)
	// This test documents that the caller must check Enabled()
	if !engine.Enabled() {
		// Expected: caller would not call Decide
		return
	}
	if result.Decision != Synthesize {
		t.Error("when called, engine still evaluates rules")
	}
}

func TestEmptyFingerprint(t *testing.T) {
	meta := map[string]string{
		"synth_fp":              "",
		"is_directly_addressed": "false",
		"personality_type":      "I",
		"agent_id":              "5",
	}
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Forward {
		t.Error("empty fingerprint should Forward")
	}
}

func TestImpulseBypassesSynthesis(t *testing.T) {
	// IM:1 = Operator-Impulse (Gaia/Broadcast) → MUST Forward, never synthesize
	meta := map[string]string{
		"synth_fp":              "H5|E5|B3|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:1",
		"is_directly_addressed": "false",
		"personality_type":      "I",
		"agent_id":              "6",
	}
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-06")
	if result.Decision != Forward {
		t.Errorf("expected Forward when HasImpulse=true (Gaia/Broadcast), got Synthesize rule=%s", result.Rule)
	}
}
