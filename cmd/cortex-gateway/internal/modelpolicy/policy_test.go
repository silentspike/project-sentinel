package modelpolicy

import (
	"encoding/json"
	"testing"
)

func TestPolicyJSONDualForm(t *testing.T) {
	for _, legacy := range []string{"", "haiku"} {
		var policy Policy
		if err := json.Unmarshal([]byte(`"`+legacy+`"`), &policy); err != nil {
			t.Fatal(err)
		}
		encoded, err := json.Marshal(policy)
		if err != nil || string(encoded) != `"`+legacy+`"` {
			t.Fatalf("legacy round trip=%s err=%v", encoded, err)
		}
	}

	const tiered = `{"providers":{"local-loop":{"tier1":"one","tier2":"two","tier3":"three"}}}`
	var policy Policy
	if err := json.Unmarshal([]byte(tiered), &policy); err != nil {
		t.Fatal(err)
	}
	if model, err := policy.Model("local-loop", 2); err != nil || model != "two" {
		t.Fatalf("model=%q err=%v", model, err)
	}
	clone := policy.Clone()
	if model, err := clone.Model("local-loop", 3); err != nil || model != "three" {
		t.Fatalf("clone model=%q err=%v", model, err)
	}
}

func TestPolicyRejectsInvalidForms(t *testing.T) {
	for _, input := range []string{
		`"opus"`,
		`null`,
		`[]`,
		`{"providers":{}}`,
		`{"providers":{"local-loop":{"tier1":"one","tier2":"two"}}}`,
		`{"providers":{"local-loop":{"tier1":"one","tier2":"two","tier3":"three","extra":"no"}}}`,
		`{"providers":{"local-loop":{"tier1":"one","tier2":"two","tier3":"three"}},"extra":true}`,
		`{"providers":{"local-loop":{"tier1":"one","tier2":"two","tier3":"three"}}} {}`,
	} {
		var policy Policy
		if err := json.Unmarshal([]byte(input), &policy); err == nil {
			t.Fatalf("invalid policy accepted: %s", input)
		}
	}
}
