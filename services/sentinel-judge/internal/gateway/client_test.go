package gateway

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestClientChat(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			t.Errorf("expected POST, got %s", r.Method)
		}
		if r.URL.Path != "/internal/llm" {
			t.Errorf("unexpected path: %s", r.URL.Path)
		}
		if got := r.Header.Get("Authorization"); got != "Bearer judge-test-credential" {
			t.Errorf("authorization = %q", got)
		}

		var req ChatRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			t.Fatalf("decode request: %v", err)
		}
		if len(req.Messages) != 2 {
			t.Errorf("expected 2 messages, got %d", len(req.Messages))
		}
		if req.Messages[0].Role != "system" {
			t.Errorf("first message role = %q, want system", req.Messages[0].Role)
		}

		resp := ChatResponse{
			Content:    `{"phrases":["Guten Morgen"],"formality":0.7}`,
			Model:      "qwen3:7b",
			TokensUsed: 150,
		}
		w.Header().Set("Content-Type", "application/json")
		if err := json.NewEncoder(w).Encode(resp); err != nil {
			t.Errorf("encode response: %v", err)
		}
	}))
	defer server.Close()

	client := NewClient(ClientConfig{
		URL:         server.URL,
		Model:       "qwen3:7b",
		Temperature: 0.2,
		MaxTokens:   500,
		Timeout:     5 * time.Second,
		Credential:  "judge-test-credential",
	})

	result, err := client.Chat(context.Background(),
		"You are a judge analyzing agent behavior.",
		"Analyze these messages: ...")
	if err != nil {
		t.Fatalf("Chat: %v", err)
	}

	if result == "" {
		t.Error("expected non-empty response")
	}
}

func TestClientChatServerError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "internal error", http.StatusInternalServerError)
	}))
	defer server.Close()

	client := NewClient(ClientConfig{
		URL:        server.URL,
		Timeout:    5 * time.Second,
		Credential: "judge-test-credential",
	})

	_, err := client.Chat(context.Background(), "system", "user")
	if err == nil {
		t.Error("expected error for server error response")
	}
}

func TestClientChatRejectsMissingCredentialBeforeNetwork(t *testing.T) {
	called := false
	server := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		called = true
	}))
	defer server.Close()

	client := NewClient(ClientConfig{URL: server.URL})
	if _, err := client.Chat(context.Background(), "system", "user"); err == nil {
		t.Fatal("missing caller credential accepted")
	}
	if called {
		t.Fatal("client reached the network without a caller credential")
	}
}
