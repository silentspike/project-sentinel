package proxy

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
)

type queuedStatusTestProvider struct {
	*subscriptionTestProvider
	mu     sync.Mutex
	status error
}

func (p *queuedStatusTestProvider) CurrentProviderError() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.status
}

func (p *queuedStatusTestProvider) setStatus(err error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.status = err
}

func queuedTestAdmission(t *testing.T, callbacks *atomic.Int32) *SubscriptionAdmission {
	t.Helper()
	return queuedTestAdmissionWithClaim(t, callbacks, nil)
}

func queuedTestAdmissionWithClaim(t *testing.T, callbacks *atomic.Int32, onClaim func()) *SubscriptionAdmission {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		callbacks.Add(1)
		if r.URL.Path != "/operator/workflow/subscription-dispatch" || r.Header.Get("Authorization") != "Bearer test-operator" {
			t.Error("unexpected operator claim identity")
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		var claim subscriptionDispatch
		if err := json.NewDecoder(r.Body).Decode(&claim); err != nil {
			t.Error(err)
			w.WriteHeader(http.StatusBadRequest)
			return
		}
		if onClaim != nil {
			onClaim()
		}
		_ = json.NewEncoder(w).Encode(subscriptionDispatchReceipt{SchemaVersion: claim.SchemaVersion,
			AllowanceID: claim.AllowanceID, RequestID: claim.RequestID, RequestDigest: claim.RequestDigest,
			DeadlineUnixMS: time.Now().Add(time.Minute).UnixMilli()})
	}))
	t.Cleanup(server.Close)
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator")
	if err != nil {
		t.Fatal(err)
	}
	return admission
}

func assertQueuedQuotaAdmission(t *testing.T, err, quota error) {
	t.Helper()
	var admissionErr *ProviderAdmissionError
	var providerErr *ProviderError
	if !errors.As(err, &admissionErr) || !errors.Is(err, quota) ||
		!errors.As(err, &providerErr) || providerErr.StatusCode != http.StatusTooManyRequests {
		t.Fatalf("known quota lost pre-I/O classification: %T %v", err, err)
	}
}

func TestQueuedProviderKnownQuotaDoesNotQueueOrClaim(t *testing.T) {
	for _, urgent := range []bool{false, true} {
		name := "ordinary"
		if urgent {
			name = "urgent"
		}
		t.Run(name, func(t *testing.T) {
			queue := forwardqueue.NewManager(1)
			release, err := queue.Acquire(context.Background())
			if err != nil {
				t.Fatal(err)
			}
			defer release()
			quota := &ProviderError{StatusCode: http.StatusTooManyRequests, Message: "usage limit active"}
			provider := &queuedStatusTestProvider{subscriptionTestProvider: &subscriptionTestProvider{}, status: quota}
			var callbacks atomic.Int32
			wrapped := NewSubscriptionQueuedProvider(provider, queue, queuedTestAdmission(t, &callbacks))
			req := subscriptionTestRequest()
			if urgent {
				req.Metadata["hierarchy_tier"] = "2"
				req.Metadata["is_directly_addressed"] = "true"
			}
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			_, err = wrapped.Send(ctx, req)
			assertQueuedQuotaAdmission(t, err, quota)
			if callbacks.Load() != 0 || provider.calls.Load() != 0 {
				t.Fatalf("blocked request claimed=%d sent=%d", callbacks.Load(), provider.calls.Load())
			}
			if stats := queue.Stats(); stats.Active != 1 || stats.Depth != 0 {
				t.Fatalf("known quota touched occupied queue: %+v", stats)
			}
		})
	}
}

func TestQueuedProviderQuotaWhileQueuedDoesNotClaimOrDispatch(t *testing.T) {
	queue := forwardqueue.NewManager(1)
	release, err := queue.Acquire(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if release != nil {
			release()
		}
	}()
	provider := &queuedStatusTestProvider{subscriptionTestProvider: &subscriptionTestProvider{}}
	var callbacks atomic.Int32
	wrapped := NewSubscriptionQueuedProvider(provider, queue, queuedTestAdmission(t, &callbacks))
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	done := make(chan error, 1)
	go func() {
		_, err := wrapped.Send(ctx, subscriptionTestRequest())
		done <- err
	}()
	for queue.Stats().Depth != 1 {
		if ctx.Err() != nil {
			t.Fatal("request did not enter queue")
		}
		time.Sleep(time.Millisecond)
	}
	quota := &ProviderError{StatusCode: http.StatusTooManyRequests, Message: "usage limit active"}
	provider.setStatus(quota)
	release()
	release = nil
	select {
	case err := <-done:
		assertQueuedQuotaAdmission(t, err, quota)
	case <-ctx.Done():
		t.Fatal("queued request did not finish")
	}
	if callbacks.Load() != 0 || provider.calls.Load() != 0 {
		t.Fatalf("quota race claimed=%d sent=%d", callbacks.Load(), provider.calls.Load())
	}
	if stats := queue.Stats(); stats.Active != 0 || stats.Depth != 0 {
		t.Fatalf("quota race leaked queue capacity: %+v", stats)
	}
}

func TestQueuedProviderNonReporterClaimsAndDispatchesNormally(t *testing.T) {
	provider := &subscriptionTestProvider{}
	var callbacks atomic.Int32
	queue := forwardqueue.NewManager(1)
	wrapped := NewSubscriptionQueuedProvider(provider, queue, queuedTestAdmission(t, &callbacks))
	if _, ok := wrapped.(ProviderStatusReporter); ok {
		t.Fatal("wrapper invented status reporting")
	}
	response, err := wrapped.Send(context.Background(), subscriptionTestRequest())
	if err != nil || response == nil || callbacks.Load() != 1 || provider.calls.Load() != 1 {
		t.Fatalf("nonreporter response=%v err=%v claims=%d sends=%d", response, err, callbacks.Load(), provider.calls.Load())
	}
	if stats := queue.Stats(); stats.Active != 0 || stats.Depth != 0 {
		t.Fatalf("normal dispatch leaked queue capacity: %+v", stats)
	}
}

func TestQueuedProviderFirstActualQuotaFailureUnchanged(t *testing.T) {
	quota := &ProviderError{StatusCode: http.StatusTooManyRequests, Message: "usage limit active"}
	provider := &pipelineMockProvider{name: CodexCLIProviderName, err: quota}
	var callbacks atomic.Int32
	wrapped := NewSubscriptionQueuedProvider(provider, forwardqueue.NewManager(1), queuedTestAdmission(t, &callbacks))
	_, err := wrapped.Send(context.Background(), subscriptionTestRequest())
	var admissionErr *ProviderAdmissionError
	if err != quota || errors.As(err, &admissionErr) || callbacks.Load() != 1 || provider.calls != 1 {
		t.Fatalf("actual failure reclassified: err=%v claims=%d sends=%d", err, callbacks.Load(), provider.calls)
	}
}

func TestQueuedProviderQuotaDuringClaimRemainsRawProviderFailure(t *testing.T) {
	fixture, callsPath := newCodexCLIFailureFixture(t, "", "usage limit", 1)
	provider := fixture.provider
	if err := provider.CurrentProviderError(); err != nil {
		t.Fatalf("provider already blocked before claim: %v", err)
	}
	var callbacks atomic.Int32
	admission := queuedTestAdmissionWithClaim(t, &callbacks, func() {
		if err := provider.CurrentProviderError(); err != nil {
			t.Errorf("quota established before claim callback: %v", err)
		}
		// Model another in-flight Send recording quota after durable admission
		// has started, not an outcome eligible for pre-I/O reclassification.
		provider.rememberUsageLimit(codexCLIProcessError("usage limit"))
	})
	queue := forwardqueue.NewManager(1)
	wrapped := NewSubscriptionQueuedProvider(provider, queue, admission)
	response, err := wrapped.Send(context.Background(), subscriptionTestRequest())
	assertCodexCLIQuotaError(t, err)
	if response != nil || callbacks.Load() != 1 {
		t.Fatalf("claim race response=%v claims=%d", response, callbacks.Load())
	}
	if _, err := os.Stat(callsPath); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("quota established during claim spawned a subprocess: %v", err)
	}
	if stats := queue.Stats(); stats.Active != 0 || stats.Depth != 0 || len(provider.sem) != 0 {
		t.Fatalf("claim race leaked queue or provider capacity: queue=%+v slots=%d", stats, len(provider.sem))
	}
}

func TestQueuedProviderNonReporterCapabilityCombinationsDoNotAdvertiseStatus(t *testing.T) {
	plain := &mockProvider{name: "plain"}
	readiness := &readinessMockProvider{mockProvider: plain}
	for _, tc := range []struct {
		name      string
		provider  Provider
		inventory bool
		readiness bool
	}{
		{"plain", plain, false, false},
		{"inventory", &inventoryMockProvider{mockProvider: plain}, true, false},
		{"readiness", readiness, false, true},
		{"both", &inventoryReadinessMockProvider{readinessMockProvider: readiness}, true, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			wrapped := NewQueuedProvider(tc.provider, forwardqueue.NewManager(1))
			if _, ok := wrapped.(ProviderStatusReporter); ok {
				t.Fatal("nonreporter wrapper invented status capability")
			}
			_, hasInventory := wrapped.(ModelInventoryProvider)
			_, hasReadiness := wrapped.(ProviderReadinessChecker)
			if hasInventory != tc.inventory || hasReadiness != tc.readiness {
				t.Fatal("nonreporter wrapper changed inventory or readiness capability")
			}
		})
	}
}

func TestQueuedProviderPreservesStatusAndOptionalCapabilities(t *testing.T) {
	quota := errors.New("cached quota")
	reporter := &queuedStatusTestProvider{subscriptionTestProvider: &subscriptionTestProvider{}, status: quota}
	plain := &mockProvider{name: "plain"}
	inventory := &inventoryMockProvider{mockProvider: plain, models: []string{"model-a"}}
	readiness := &readinessMockProvider{mockProvider: plain}
	both := &inventoryReadinessMockProvider{readinessMockProvider: readiness, models: []string{"model-a"}}
	for _, tc := range []struct {
		name      string
		provider  Provider
		inventory bool
		readiness bool
	}{
		{"plain", &struct {
			*mockProvider
			ProviderStatusReporter
		}{plain, reporter}, false, false},
		{"inventory", &struct {
			*inventoryMockProvider
			ProviderStatusReporter
		}{inventory, reporter}, true, false},
		{"readiness", &struct {
			*readinessMockProvider
			ProviderStatusReporter
		}{readiness, reporter}, false, true},
		{"both", &struct {
			*inventoryReadinessMockProvider
			ProviderStatusReporter
		}{both, reporter}, true, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			queue := forwardqueue.NewManager(1)
			wrapped := NewQueuedProvider(tc.provider, queue)
			status, ok := wrapped.(ProviderStatusReporter)
			if !ok || status.CurrentProviderError() != quota {
				t.Fatal("wrapper lost cached status")
			}
			inv, hasInventory := wrapped.(ModelInventoryProvider)
			ready, hasReadiness := wrapped.(ProviderReadinessChecker)
			if hasInventory != tc.inventory || hasReadiness != tc.readiness {
				t.Fatal("wrapper changed optional capabilities")
			}
			if hasInventory {
				models, err := inv.ModelInventory(context.Background())
				if err != nil || len(models) != 1 || models[0] != "model-a" {
					t.Fatalf("inventory delegation: %v %v", models, err)
				}
			}
			if hasReadiness && ready.ReadinessCheck(context.Background()) != nil {
				t.Fatal("readiness delegation failed")
			}
			reporter.setStatus(nil)
			if status.CurrentProviderError() != nil {
				t.Fatal("wrapper cached a stale copy of provider status")
			}
			reporter.setStatus(quota)
			if reporter.calls.Load() != 0 || queue.Stats().Active != 0 || queue.Stats().Depth != 0 {
				t.Fatal("status read dispatched or acquired queue capacity")
			}
		})
	}
}

func TestQueuedProviderDoesNotDispatchExpiredAvailableGrant(t *testing.T) {
	queue := forwardqueue.NewManager(1)
	provider := &pipelineMockProvider{name: "mock"}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, err := NewQueuedProvider(provider, queue).Send(ctx, &LLMRequest{})
	if !errors.Is(err, context.Canceled) || provider.calls != 0 {
		t.Fatalf("expired grant reached provider: err=%v calls=%d", err, provider.calls)
	}
	if stats := queue.Stats(); stats.Active != 0 || stats.Depth != 0 {
		t.Fatalf("expired grant leaked queue capacity: %+v", stats)
	}
}

func TestQueuedProviderDeadlineWhileWaitingDoesNotDispatchOrLeakCapacity(t *testing.T) {
	queue := forwardqueue.NewManager(1)
	release, err := queue.Acquire(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	defer release()
	provider := &pipelineMockProvider{name: "mock"}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	_, err = NewQueuedProvider(provider, queue).Send(ctx, &LLMRequest{})
	if !errors.Is(err, context.DeadlineExceeded) || provider.calls != 0 {
		t.Fatalf("expired waiter reached provider: err=%v calls=%d", err, provider.calls)
	}
	if stats := queue.Stats(); stats.Active != 1 || stats.Depth != 0 {
		t.Fatalf("expired waiter changed the existing lease or leaked a waiter: %+v", stats)
	}
}
