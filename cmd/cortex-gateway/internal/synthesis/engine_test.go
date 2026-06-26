package synthesis

import (
	"io"
	"log/slog"
	"sync"
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
	fp, err := Parse("H3|E7|B0|S0|C0|SN0|R:test|P:0|CH:0|HR:0|T:0|TMP:0|PE:I|IM:0")
	if err != nil {
		t.Fatalf("Parse partial: %v", err)
	}
	if fp.Hunger != 3 || fp.Energy != 7 {
		t.Errorf("H=%d E=%d, want H=3 E=7", fp.Hunger, fp.Energy)
	}
}

func TestBioBladderUsesModuloTarget(t *testing.T) {
	engine := NewEngine(true, nil)

	odd := engine.Decide(testMetadata("H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "5",
		"personality_type": "I",
	}), "AGENT-05")
	if odd.Decision != Synthesize {
		t.Fatal("expected Synthesize for bladder=9")
	}
	if odd.Rule != "bio_bladder" {
		t.Fatalf("rule = %q, want bio_bladder", odd.Rule)
	}
	if target := findActionTarget(odd.Actions, "move"); target != "toilette-eg-herren" {
		t.Fatalf("odd agent target = %q, want toilette-eg-herren", target)
	}

	even := engine.Decide(testMetadata("H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:E|IM:0", map[string]string{
		"agent_id":         "6",
		"personality_type": "E",
	}), "AGENT-06")
	if even.Decision != Synthesize {
		t.Fatal("expected Synthesize for even bladder=9")
	}
	if target := findActionTarget(even.Actions, "move"); target != "toilette-eg-damen" {
		t.Fatalf("even agent target = %q, want toilette-eg-damen", target)
	}
}

func TestBioHungerFires(t *testing.T) {
	meta := testMetadata("H9|E5|B3|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "5",
		"personality_type": "I",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for hunger=9")
	}
	if result.Rule != "bio_hunger" {
		t.Errorf("rule = %q, want bio_hunger", result.Rule)
	}
	if target := findActionTarget(result.Actions, "move"); target != "kueche" {
		t.Errorf("move target = %q, want kueche", target)
	}
}

func TestBioHungerBlockedByHeardMetadata(t *testing.T) {
	meta := testMetadata("H9|E5|B3|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "5",
		"personality_type": "I",
		"heard":            "Lisa sagte: Hallo",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Forward {
		t.Error("expected Forward when heard metadata is present")
	}
}

func TestBioHungerBlockedByChaos(t *testing.T) {
	meta := testMetadata("H9|E5|B3|S3|C5|SN5|R:buero-dev-1|P:0|CH:1|HR:0|T:10|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "5",
		"personality_type": "I",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Forward {
		t.Error("expected Forward when HasChaos=true")
	}
}

func TestBioBladderBlockedByAddressed(t *testing.T) {
	meta := testMetadata("H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0", map[string]string{
		"is_directly_addressed": "true",
		"personality_type":      "I",
		"agent_id":              "5",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Forward {
		t.Error("expected Forward when isAddressed=true")
	}
}

func TestRoutineIdleAlone(t *testing.T) {
	meta := testMetadata("H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:14|TMP:0|PE:E|IM:0", map[string]string{
		"agent_id":         "10",
		"personality_type": "E",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for idle alone")
	}
	if result.Rule != "routine_idle_alone" {
		t.Errorf("rule = %q, want routine_idle_alone", result.Rule)
	}
}

func TestRoutineIdleWithPresenceFromFingerprint(t *testing.T) {
	meta := testMetadata("H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:3|CH:0|HR:0|T:14|TMP:0|PE:E|IM:0", map[string]string{
		"agent_id":         "10",
		"personality_type": "E",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for idle with presence")
	}
	if result.Rule != "routine_idle_with_presence" {
		t.Errorf("rule = %q, want routine_idle_with_presence", result.Rule)
	}
}

func TestRoutineIdleWithPresenceFromMetadata(t *testing.T) {
	meta := testMetadata("H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:14|TMP:0|PE:E|IM:0", map[string]string{
		"agent_id":         "10",
		"personality_type": "E",
		"presence":         "Lisa (Konzept), Thomas (Review)",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for metadata-derived presence")
	}
	if result.Rule != "routine_idle_with_presence" {
		t.Errorf("rule = %q, want routine_idle_with_presence", result.Rule)
	}
}

func TestCircadianMorning(t *testing.T) {
	meta := testMetadata("H3|E7|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:6|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "1",
		"personality_type": "I",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for circadian_morning")
	}
	if result.Rule != "circadian_morning" {
		t.Errorf("rule = %q, want circadian_morning", result.Rule)
	}
}

func TestCircadianFallsBackToMetadata(t *testing.T) {
	meta := testMetadata("H3|E7|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:0|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "1",
		"personality_type": "I",
		"circadian":        "06:30 Uhr",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for metadata-derived circadian morning")
	}
	if result.Rule != "circadian_morning" {
		t.Errorf("rule = %q, want circadian_morning", result.Rule)
	}
}

func TestCircadianLunch(t *testing.T) {
	meta := testMetadata("H7|E5|B3|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:12|TMP:0|PE:E|IM:0", map[string]string{
		"agent_id":         "2",
		"personality_type": "E",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for circadian_lunch")
	}
	if result.Rule != "circadian_lunch" {
		t.Errorf("rule = %q, want circadian_lunch", result.Rule)
	}
	if target := findActionTarget(result.Actions, "move"); target != "kueche" {
		t.Errorf("move target = %q, want kueche", target)
	}
}

func TestPhysicsTempHigh(t *testing.T) {
	meta := testMetadata("H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:14|TMP:1|PE:I|IM:0", map[string]string{
		"agent_id":         "3",
		"personality_type": "I",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for physics_temp_high")
	}
	if result.Rule != "physics_temp_high" {
		t.Errorf("rule = %q, want physics_temp_high", result.Rule)
	}
	if target := findActionTarget(result.Actions, "tool_use"); target != "open_window" {
		t.Errorf("tool target = %q, want open_window", target)
	}
}

func TestPhysicsNoiseHighFromAcousticMetadata(t *testing.T) {
	meta := testMetadata("H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:14|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "3",
		"personality_type": "I",
		"acoustic":         "Es ist laut (72 dB). Konzentration faellt schwer.",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Synthesize {
		t.Fatal("expected Synthesize for physics_noise_high")
	}
	if result.Rule != "physics_noise_high" {
		t.Errorf("rule = %q, want physics_noise_high", result.Rule)
	}
}

func TestPersonalityTemplates(t *testing.T) {
	engine := NewEngine(true, nil)

	resultI := engine.Decide(testMetadata("H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:14|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "5",
		"personality_type": "I",
	}), "AGENT-I")
	resultE := engine.Decide(testMetadata("H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:0|CH:0|HR:0|T:14|TMP:0|PE:E|IM:0", map[string]string{
		"agent_id":         "6",
		"personality_type": "E",
	}), "AGENT-E")

	if resultI.Content == resultE.Content {
		t.Errorf("I and E templates should differ, both got: %q", resultI.Content)
	}
}

func TestDisabledEngineForwards(t *testing.T) {
	meta := testMetadata("H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "5",
		"personality_type": "I",
	})
	engine := NewEngine(false, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if !engine.Enabled() {
		return
	}
	if result.Decision != Synthesize {
		t.Error("when called, engine still evaluates rules")
	}
}

func TestEmptyFingerprint(t *testing.T) {
	meta := testMetadata("", nil)
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-TEST")
	if result.Decision != Forward {
		t.Error("empty fingerprint should Forward")
	}
}

func TestImpulseBypassesSynthesis(t *testing.T) {
	meta := testMetadata("H5|E5|B3|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:1", map[string]string{
		"agent_id":         "6",
		"personality_type": "I",
	})
	engine := NewEngine(true, nil)
	result := engine.Decide(meta, "AGENT-06")
	if result.Decision != Forward {
		t.Errorf("expected Forward when HasImpulse=true (Gaia/Broadcast), got Synthesize rule=%s", result.Rule)
	}
}

func testMetadata(fp string, overrides map[string]string) map[string]string {
	meta := map[string]string{
		"synth_fp":              fp,
		"is_directly_addressed": "false",
		"personality_type":      "E",
		"agent_id":              "1",
	}
	for k, v := range overrides {
		meta[k] = v
	}
	return meta
}

func findActionTarget(actions []Action, actionType string) string {
	for _, action := range actions {
		if action.Type == actionType {
			return action.Target
		}
	}
	return ""
}

// #429: per-rule live toggle is effective in Decide.
func TestPerRuleToggleAffectsDecide(t *testing.T) {
	// H9, P:0 -> bio_hunger by default (it precedes the routine catch-all rules).
	meta := testMetadata("H9|E5|B3|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "5",
		"personality_type": "I",
	})
	engine := NewEngine(true, nil)

	if got := engine.Decide(meta, "AGENT-TEST"); got.Decision != Synthesize || got.Rule != "bio_hunger" {
		t.Fatalf("default: decision=%v rule=%q, want Synthesize/bio_hunger", got.Decision, got.Rule)
	}

	// Disabling bio_hunger makes the engine skip it; the next matching rule
	// (routine_idle_alone for PresenceCount==0) fires -> the toggle is effective.
	if !engine.SetRuleEnabled("bio_hunger", false) {
		t.Fatal("SetRuleEnabled(bio_hunger,false) returned false for a known rule")
	}
	if got := engine.Decide(meta, "AGENT-TEST"); got.Rule == "bio_hunger" {
		t.Fatalf("after disable: rule still bio_hunger, the toggle had no effect")
	}

	// Disabling the catch-all routine rule too drives the same fingerprint to Forward.
	engine.SetRuleEnabled("routine_idle_alone", false)
	if got := engine.Decide(meta, "AGENT-TEST"); got.Decision != Forward {
		t.Fatalf("with bio_hunger+routine_idle_alone disabled: decision=%v rule=%q, want Forward", got.Decision, got.Rule)
	}

	// Re-enabling bio_hunger restores synthesis.
	engine.SetRuleEnabled("bio_hunger", true)
	if got := engine.Decide(meta, "AGENT-TEST"); got.Decision != Synthesize || got.Rule != "bio_hunger" {
		t.Fatalf("after re-enable: decision=%v rule=%q, want Synthesize/bio_hunger", got.Decision, got.Rule)
	}
}

// #429: RuleStates lists all default rules (enabled) in order; unknown toggle is rejected.
func TestRuleStatesAndUnknownToggle(t *testing.T) {
	engine := NewEngine(true, nil)
	states := engine.RuleStates()
	if len(states) != 10 {
		t.Fatalf("RuleStates len = %d, want 10", len(states))
	}
	for _, s := range states {
		if !s.Enabled {
			t.Errorf("rule %q should default to enabled", s.Name)
		}
	}
	if states[0].Name != "bio_bladder" {
		t.Errorf("states[0] = %q, want bio_bladder (DefaultRules order preserved)", states[0].Name)
	}
	if engine.SetRuleEnabled("does_not_exist", false) {
		t.Error("SetRuleEnabled for an unknown rule returned true, want false")
	}
}

// #429: the per-rule gating (RWMutex + map lookup per rule) must not regress the hot Decide
// path. all-disabled is the worst case (10 isRuleEnabled RLocks, then Forward); all-enabled
// matches the first bio rule. A negligible delta proves the gating is proportional.
func BenchmarkDecide(b *testing.B) {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	meta := testMetadata("H9|E5|B3|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "5",
		"personality_type": "I",
	})
	b.Run("all-enabled", func(b *testing.B) {
		e := NewEngine(true, logger)
		b.ResetTimer()
		for i := 0; i < b.N; i++ {
			_ = e.Decide(meta, "AGENT-05")
		}
	})
	b.Run("all-disabled", func(b *testing.B) {
		e := NewEngine(true, logger)
		for _, s := range e.RuleStates() {
			e.SetRuleEnabled(s.Name, false)
		}
		b.ResetTimer()
		for i := 0; i < b.N; i++ {
			_ = e.Decide(meta, "AGENT-05")
		}
	})
}

// #429: live toggling must be race-free against concurrent Decide reads (run with -race).
func TestRuleToggleConcurrentWithDecide(t *testing.T) {
	engine := NewEngine(true, nil)
	meta := testMetadata("H9|E5|B3|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0", map[string]string{
		"agent_id":         "5",
		"personality_type": "I",
	})
	var wg sync.WaitGroup
	for i := 0; i < 8; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 500; j++ {
				engine.Decide(meta, "AGENT-TEST")
			}
		}()
	}
	for i := 0; i < 4; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 500; j++ {
				engine.SetRuleEnabled("bio_hunger", j%2 == 0)
				_ = engine.RuleStates()
			}
		}()
	}
	wg.Wait()
}
