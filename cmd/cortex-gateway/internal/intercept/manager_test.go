package intercept

import (
	"context"
	"testing"
	"time"
)

func TestAwaitRequestDecisionResolve(t *testing.T) {
	mgr := NewManager()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()

	done := make(chan RequestDecision, 1)
	go func() {
		decision, ok := mgr.AwaitRequestDecision(ctx, PendingRequest{
			ID:        "req-1",
			RoomID:    "room-1",
			AgentName: "AGENT-01",
			Reason:    "manual_mode",
			CreatedAt: time.Now(),
		})
		if !ok {
			t.Errorf("expected manual decision to resolve")
			return
		}
		done <- decision
	}()

	time.Sleep(20 * time.Millisecond)
	if ok := mgr.ResolveRequest("req-1", Modify("manual edit", "\n[KONTEXT] foo [/KONTEXT]")); !ok {
		t.Fatal("expected resolve to succeed")
	}

	select {
	case decision := <-done:
		if decision.Action != RequestModify {
			t.Fatalf("action = %q, want modify", decision.Action)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for resolved decision")
	}
}

func TestPendingSortedByCreatedAt(t *testing.T) {
	mgr := NewManager()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	go func() {
		_, _ = mgr.AwaitRequestDecision(ctx, PendingRequest{
			ID:        "req-2",
			RoomID:    "room-1",
			AgentName: "AGENT-02",
			Reason:    "manual_mode",
			CreatedAt: time.Now().Add(time.Second),
		})
	}()
	go func() {
		_, _ = mgr.AwaitRequestDecision(ctx, PendingRequest{
			ID:        "req-1",
			RoomID:    "room-1",
			AgentName: "AGENT-01",
			Reason:    "manual_mode",
			CreatedAt: time.Now(),
		})
	}()

	time.Sleep(20 * time.Millisecond)
	pending := mgr.Pending()
	if len(pending) != 2 {
		t.Fatalf("pending len = %d, want 2", len(pending))
	}
	if pending[0].ID != "req-1" {
		t.Fatalf("first pending = %q, want req-1", pending[0].ID)
	}
}

func TestAwaitResponseDecisionResolve(t *testing.T) {
	mgr := NewResponseManager()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()

	done := make(chan ResponseDecision, 1)
	go func() {
		decision, ok := mgr.AwaitDecision(ctx, PendingResponse{
			ID:        "resp-1",
			RoomID:    "room-1",
			AgentName: "AGENT-01",
			Provider:  "mock",
			Content:   "original",
			CreatedAt: time.Now(),
		})
		if !ok {
			t.Errorf("expected response decision to resolve")
			return
		}
		done <- decision
	}()

	time.Sleep(20 * time.Millisecond)
	if ok := mgr.Resolve("resp-1", ResponseDecision{Action: ResponseReplace, Reason: "manual", Content: "replaced"}); !ok {
		t.Fatal("expected response resolve to succeed")
	}

	select {
	case decision := <-done:
		if decision.Action != ResponseReplace {
			t.Fatalf("action = %q, want replace", decision.Action)
		}
		if decision.Content != "replaced" {
			t.Fatalf("content = %q, want replaced", decision.Content)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for response decision")
	}
}
