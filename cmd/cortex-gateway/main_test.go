package main

import (
	"context"
	"testing"
	"time"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/sequencing"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/synthesis"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/ticksync"
)

func TestDefaultPrimaryProviderFallsBackToClaudeCodeWithoutAPIKey(t *testing.T) {
	t.Setenv("CORTEX_PRIMARY_PROVIDER", "")
	t.Setenv("ANTHROPIC_API_KEY", "")

	if got := defaultPrimaryProvider(); got != "claude-code" {
		t.Fatalf("defaultPrimaryProvider() = %q, want %q", got, "claude-code")
	}
}

func TestApplyTrafficRuntimeConfigPropagatesRuntimeValues(t *testing.T) {
	cfg := control.NewConfig("claude-code")
	if err := cfg.Update(map[string]any{
		"synthesis_enabled":       true,
		"sequencing_enabled":      true,
		"tick_sync_enabled":       true,
		"tick_sync_timeout_ms":    25,
		"p3_timeout_ms":           30,
		"max_forward_concurrency": 2,
	}); err != nil {
		t.Fatalf("config update: %v", err)
	}

	synth := synthesis.NewEngine(false, nil)
	sequencer := sequencing.NewSequencer(time.Second, false, nil)
	tickSync := ticksync.NewBuffer(time.Second, false, nil)
	defer tickSync.Stop()
	queue := forwardqueue.NewManager(1)

	applyTrafficRuntimeConfig(cfg.Get(), synth, sequencer, tickSync, queue)

	if !synth.Enabled() {
		t.Fatal("synthesis engine should be enabled after runtime apply")
	}
	if !sequencer.Enabled() {
		t.Fatal("sequencer should be enabled after runtime apply")
	}
	if !tickSync.Enabled() {
		t.Fatal("tick sync should be enabled after runtime apply")
	}

	release1, err := queue.Acquire(context.Background())
	if err != nil {
		t.Fatalf("acquire 1: %v", err)
	}
	defer release1()
	release2, err := queue.Acquire(context.Background())
	if err != nil {
		t.Fatalf("acquire 2: %v", err)
	}
	defer release2()

	if stats := queue.Stats(); stats.Active != 2 {
		t.Fatalf("queue active = %d, want 2", stats.Active)
	}

	sequencer.MarkP1Active("room-1", "req-1", "AGENT-01")
	start := time.Now()
	_, _, ok := sequencer.WaitForP1("room-1")
	if ok {
		t.Fatal("expected runtime-updated sequencer timeout to fire")
	}
	if elapsed := time.Since(start); elapsed > 250*time.Millisecond {
		t.Fatalf("sequencer timeout not applied, waited %v", elapsed)
	}
}
