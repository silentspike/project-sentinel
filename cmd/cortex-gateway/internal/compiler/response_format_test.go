package compiler

import (
	"reflect"
	"strings"
	"sync"
	"testing"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/capability"
)

func TestResponseFormatCopiesOnlyFormatBlock(t *testing.T) {
	c := NewWithAssembler(NewTOMLLoader(setupTestAgent(t)), capability.New())
	for _, sources := range []bool{false, true} {
		original := c.CompileStructured("claude", "Employee", "Developer", StructuredPerception{})
		if sources {
			var err error
			original, err = c.CompileStructuredFromSources(1, "claude", EvolutionData{}, StructuredPerception{})
			if err != nil {
				t.Fatal(err)
			}
		}
		before := append([]CompiledSystemBlock(nil), original.SystemBlocks...)
		changed := WithResponseFormat(original, "GOVERNED-FORMAT")
		if !reflect.DeepEqual(original.SystemBlocks, before) {
			t.Fatal("input blocks mutated")
		}
		if changed.LegacySystemPrompt != flattenStructuredBlocks(changed.SystemBlocks) {
			t.Fatal("flattened format stale")
		}
		for i, block := range before {
			if block.Tag == "action-format" {
				if changed.SystemBlocks[i].Text != "<action-format>\nGOVERNED-FORMAT\n</action-format>" {
					t.Fatal("format not replaced")
				}
			} else if !reflect.DeepEqual(block, changed.SystemBlocks[i]) {
				t.Errorf("non-format block changed: %s", block.Tag)
			}
		}
	}
}

func TestResponseFormatCachedDNAIsolation(t *testing.T) {
	loader := NewTOMLLoader(setupTestAgent(t))
	c := NewWithAssembler(loader, capability.New())
	dna, err := loader.Load(1)
	if err != nil {
		t.Fatal(err)
	}
	original := *dna
	ambient, err := c.CompileFromSources(1, "claude", EvolutionData{}, "PERCEPTION")
	if err != nil {
		t.Fatal(err)
	}
	var wg sync.WaitGroup
	for range 12 {
		wg.Go(func() {
			governed, err := c.CompileFromSourcesWithResponseFormat(1, "claude", EvolutionData{}, "PERCEPTION", "GOVERNED-FORMAT")
			if err != nil || !strings.Contains(governed, "GOVERNED-FORMAT") || strings.Contains(governed, "Moegliche Aktionen:") {
				t.Errorf("governed prompt: %v %s", err, governed)
			}
			after, err := c.CompileFromSources(1, "claude", EvolutionData{}, "PERCEPTION")
			if err != nil || after != ambient {
				t.Errorf("ambient prompt changed: %v", err)
			}
		})
	}
	wg.Wait()
	after, err := loader.Load(1)
	if err != nil {
		t.Fatal(err)
	}
	if after != dna || !reflect.DeepEqual(*after, original) {
		t.Fatal("shared DNA cache changed")
	}
}

func TestResponseFormatSurvivesLegacyDistillationAndTruncation(t *testing.T) {
	c := NewWithAssembler(NewTOMLLoader(setupTestAgent(t)), capability.New())
	const responseFormat = "Return only the governed JSON contract. Choose independently."
	const budget = 2000
	for _, prefix := range []string{"UNPRIORITIZED: ", "KOERPER: "} {
		t.Run(prefix, func(t *testing.T) {
			perception := prefix + strings.Repeat("PERCEPTION-SENTINEL ", 250)
			ambient, err := c.CompileFromSources(1, "ollama", EvolutionData{}, perception)
			if err != nil {
				t.Fatal(err)
			}
			governed, err := c.CompileFromSourcesWithResponseFormat(1, "ollama", EvolutionData{}, perception, responseFormat)
			if err != nil {
				t.Fatal(err)
			}
			if strings.Count(governed, responseFormat) != 1 || len(governed) > budget {
				t.Fatalf("contract lost or budget exceeded: %d bytes, %q", len(governed), governed)
			}
			if strings.Contains(governed, "Wenn Kollegen anwesend") || !strings.Contains(governed, "Du bist Thomas Mueller") {
				t.Fatal("expected forced DNA distillation with identity retained")
			}
			if prefix == "KOERPER: " && len(governed) != budget {
				t.Fatal("expected forced final truncation as well as distillation")
			}
			after, err := c.CompileFromSources(1, "ollama", EvolutionData{}, perception)
			if err != nil || after != ambient {
				t.Fatalf("ambient compilation changed: %v", err)
			}
		})
	}
}
