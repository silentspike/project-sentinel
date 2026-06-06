package proxy

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"strconv"
	"strings"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/guardrails"
)

const defaultLocalLoopModel = "local-loop"

// LocalLoopConfig configures the deterministic token-free provider.
type LocalLoopConfig struct {
	Name         string
	Model        string
	ScenarioPath string
}

// LocalLoopProvider returns Anthropic-compatible synthetic responses without
// opening a network connection or spawning a subprocess.
type LocalLoopProvider struct {
	name     string
	model    string
	scenario *localLoopScenario
}

type localLoopBrainInput struct {
	AgentID         string
	Tick            string
	RoomID          string
	Heard           string
	LastUser        string
	PersonalityType string
}

type localLoopGenerated struct {
	ID           string
	Model        string
	Content      string
	InputTokens  int
	OutputTokens int
}

type localLoopScenario struct {
	exact    map[string]string
	agent    map[string]string
	wildcard string
}

type localLoopScenarioLine struct {
	AgentID string `json:"agent_id"`
	Tick    *int64 `json:"tick,omitempty"`
	Content string `json:"content"`
}

// NewLocalLoopProvider creates the local-loop provider and validates an optional scenario file.
func NewLocalLoopProvider(cfg LocalLoopConfig) (*LocalLoopProvider, error) {
	name := strings.TrimSpace(cfg.Name)
	if name == "" {
		name = LocalLoopProviderName
	}
	model := strings.TrimSpace(cfg.Model)
	if model == "" {
		model = defaultLocalLoopModel
	}

	scenario, err := loadLocalLoopScenario(cfg.ScenarioPath)
	if err != nil {
		return nil, err
	}

	return &LocalLoopProvider{
		name:     name,
		model:    model,
		scenario: scenario,
	}, nil
}

func (p *LocalLoopProvider) Name() string { return p.name }

func (p *LocalLoopProvider) Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	default:
	}

	generated := p.generate(req)
	guardrails.RecordRuntimeSynthesisSavings()

	return &LLMResponse{
		Content:      generated.Content,
		Model:        generated.Model,
		TokensUsed:   generated.InputTokens + generated.OutputTokens,
		InputTokens:  generated.InputTokens,
		OutputTokens: generated.OutputTokens,
		FinishReason: "end_turn",
	}, nil
}

func (p *LocalLoopProvider) StreamHTTP(ctx context.Context, req *LLMRequest, w http.ResponseWriter) error {
	select {
	case <-ctx.Done():
		return ctx.Err()
	default:
	}

	generated := p.generate(req)
	w.Header().Set("Content-Type", "text/event-stream; charset=utf-8")
	w.Header().Set("Cache-Control", "no-cache")
	w.WriteHeader(http.StatusOK)

	if err := writeLocalLoopSSE(w, "message_start", map[string]any{
		"type": "message_start",
		"message": map[string]any{
			"id":            generated.ID,
			"type":          "message",
			"role":          "assistant",
			"model":         generated.Model,
			"content":       []any{},
			"stop_reason":   nil,
			"stop_sequence": nil,
			"usage": map[string]any{
				"input_tokens":                generated.InputTokens,
				"output_tokens":               1,
				"cache_read_input_tokens":     0,
				"cache_creation_input_tokens": 0,
				"service_tier":                "standard",
			},
		},
	}); err != nil {
		return err
	}
	if err := writeLocalLoopSSE(w, "content_block_start", map[string]any{
		"type":  "content_block_start",
		"index": 0,
		"content_block": map[string]any{
			"type": "text",
			"text": "",
		},
	}); err != nil {
		return err
	}
	if err := writeLocalLoopSSE(w, "ping", map[string]string{"type": "ping"}); err != nil {
		return err
	}
	for _, chunk := range localLoopChunks(generated.Content, 48) {
		if err := writeLocalLoopSSE(w, "content_block_delta", map[string]any{
			"type":  "content_block_delta",
			"index": 0,
			"delta": map[string]string{
				"type": "text_delta",
				"text": chunk,
			},
		}); err != nil {
			return err
		}
	}
	if err := writeLocalLoopSSE(w, "content_block_stop", map[string]any{
		"type":  "content_block_stop",
		"index": 0,
	}); err != nil {
		return err
	}
	if err := writeLocalLoopSSE(w, "message_delta", map[string]any{
		"type": "message_delta",
		"delta": map[string]any{
			"stop_reason":   "end_turn",
			"stop_sequence": nil,
		},
		"usage": map[string]int{
			"input_tokens":  generated.InputTokens,
			"output_tokens": generated.OutputTokens,
		},
	}); err != nil {
		return err
	}
	if err := writeLocalLoopSSE(w, "message_stop", map[string]string{"type": "message_stop"}); err != nil {
		return err
	}

	guardrails.RecordRuntimeSynthesisSavings()
	return nil
}

func (p *LocalLoopProvider) HealthCheck(_ context.Context) error { return nil }

func (p *LocalLoopProvider) generate(req *LLMRequest) localLoopGenerated {
	input := localLoopInput(req)
	key := strings.Join([]string{
		input.AgentID,
		input.Tick,
		input.RoomID,
		input.Heard,
		input.LastUser,
		input.PersonalityType,
	}, "\x00")
	sum := sha256.Sum256([]byte(key))

	content := ""
	if p.scenario != nil {
		content = p.scenario.lookup(input)
	}
	if strings.TrimSpace(content) == "" {
		content = localLoopPoolContent(input, sum)
	}

	model := p.model
	if req != nil && strings.TrimSpace(req.Model) != "" {
		model = strings.TrimSpace(req.Model)
	}

	inputTokens := estimateLocalLoopTokens(strings.Join([]string{
		input.AgentID,
		input.Tick,
		input.RoomID,
		input.Heard,
		input.LastUser,
		input.PersonalityType,
	}, " "))
	outputTokens := estimateLocalLoopTokens(content)

	return localLoopGenerated{
		ID:           "msg_" + hex.EncodeToString(sum[:8]),
		Model:        model,
		Content:      content,
		InputTokens:  inputTokens,
		OutputTokens: outputTokens,
	}
}

func localLoopInput(req *LLMRequest) localLoopBrainInput {
	if req == nil {
		return localLoopBrainInput{}
	}
	meta := req.Metadata
	return localLoopBrainInput{
		AgentID:         canonicalLocalLoopAgent(meta["agent_id"], meta["agent_name"]),
		Tick:            strings.TrimSpace(meta["tick"]),
		RoomID:          strings.TrimSpace(meta["room_id"]),
		Heard:           strings.TrimSpace(meta["heard"]),
		LastUser:        lastUserMessage(req.Messages),
		PersonalityType: strings.TrimSpace(meta["personality_type"]),
	}
}

func localLoopPoolContent(input localLoopBrainInput, sum [32]byte) string {
	if input.Heard != "" || input.LastUser != "" {
		return "AKTION: Chat\nZIEL: Team\nINHALT: Ich habe das gehoert und reagiere ruhig darauf."
	}

	room := input.RoomID
	if room == "" {
		room = "-"
	}
	pool := []string{
		"AKTION: Emote\nZIEL: -\nINHALT: *blickt kurz auf und arbeitet fokussiert weiter*",
		"AKTION: Move\nZIEL: kueche\nINHALT: *steht auf und geht kurz in Richtung Kueche*",
		"AKTION: Chat\nZIEL: Team\nINHALT: Ich stimme den naechsten Schritt kurz mit dem Team ab.",
		"AKTION: Work\nZIEL: " + room + "\nINHALT: *notiert den naechsten Schritt und arbeitet konzentriert weiter*",
		"AKTION: Emote\nZIEL: -\nINHALT: *atmet kurz durch und sortiert die Gedanken*",
	}
	idx := int(sum[0]) % len(pool)
	return pool[idx]
}

func lastUserMessage(messages []Message) string {
	for i := len(messages) - 1; i >= 0; i-- {
		if strings.EqualFold(strings.TrimSpace(messages[i].Role), "user") {
			return strings.TrimSpace(messages[i].Content)
		}
	}
	return ""
}

func canonicalLocalLoopAgent(agentID, agentName string) string {
	agentID = strings.TrimSpace(agentID)
	if n, err := strconv.Atoi(agentID); err == nil && n > 0 {
		return fmt.Sprintf("AGENT-%02d", n)
	}
	if agentID != "" {
		return agentID
	}
	return strings.TrimSpace(agentName)
}

func estimateLocalLoopTokens(text string) int {
	runes := []rune(strings.TrimSpace(text))
	if len(runes) == 0 {
		return 1
	}
	tokens := len(runes) / 4
	if tokens < 1 {
		return 1
	}
	return tokens
}

func loadLocalLoopScenario(path string) (*localLoopScenario, error) {
	path = strings.TrimSpace(path)
	if path == "" {
		return nil, nil
	}

	file, err := os.Open(path) //nolint:gosec // local operator-provided scenario file
	if err != nil {
		return nil, fmt.Errorf("load local-loop scenario %q: %w", path, err)
	}
	defer func() { _ = file.Close() }()

	scenario := &localLoopScenario{
		exact: make(map[string]string),
		agent: make(map[string]string),
	}
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 0, 64*1024), 2*1024*1024)
	lineNo := 0
	for scanner.Scan() {
		lineNo++
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		var entry localLoopScenarioLine
		if err := json.Unmarshal([]byte(line), &entry); err != nil {
			return nil, fmt.Errorf("invalid local-loop scenario %s:%d: %w", path, lineNo, err)
		}
		agent := strings.TrimSpace(entry.AgentID)
		content := strings.TrimSpace(entry.Content)
		if agent == "" {
			return nil, fmt.Errorf("invalid local-loop scenario %s:%d: agent_id is required", path, lineNo)
		}
		if content == "" {
			return nil, fmt.Errorf("invalid local-loop scenario %s:%d: content is required", path, lineNo)
		}
		if entry.Tick != nil && *entry.Tick < 0 {
			return nil, fmt.Errorf("invalid local-loop scenario %s:%d: tick must be non-negative", path, lineNo)
		}

		switch {
		case agent == "*":
			scenario.wildcard = content
		case entry.Tick != nil:
			scenario.exact[localLoopScenarioKey(canonicalLocalLoopAgent(agent, ""), strconv.FormatInt(*entry.Tick, 10))] = content
		default:
			scenario.agent[canonicalLocalLoopAgent(agent, "")] = content
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("read local-loop scenario %q: %w", path, err)
	}
	return scenario, nil
}

func (s *localLoopScenario) lookup(input localLoopBrainInput) string {
	if s == nil {
		return ""
	}
	if content := s.exact[localLoopScenarioKey(input.AgentID, input.Tick)]; content != "" {
		return content
	}
	if content := s.agent[input.AgentID]; content != "" {
		return content
	}
	return s.wildcard
}

func localLoopScenarioKey(agentID, tick string) string {
	return strings.TrimSpace(agentID) + "\x00" + strings.TrimSpace(tick)
}

func writeLocalLoopSSE(w http.ResponseWriter, event string, payload any) error {
	data, err := json.Marshal(payload)
	if err != nil {
		return err
	}
	if _, err := fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, data); err != nil {
		return err
	}
	if flusher, ok := w.(http.Flusher); ok {
		flusher.Flush()
	}
	return nil
}

func localLoopChunks(text string, maxRunes int) []string {
	if maxRunes <= 0 {
		maxRunes = 48
	}
	runes := []rune(text)
	if len(runes) == 0 {
		return []string{""}
	}
	chunks := make([]string, 0, (len(runes)+maxRunes-1)/maxRunes)
	for len(runes) > 0 {
		n := maxRunes
		if len(runes) < n {
			n = len(runes)
		}
		chunks = append(chunks, string(runes[:n]))
		runes = runes[n:]
	}
	return chunks
}
