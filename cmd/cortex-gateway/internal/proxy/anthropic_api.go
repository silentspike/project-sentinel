package proxy

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
)

const anthropicMessagesPath = "/v1/messages"

type anthropicInboundRequest struct {
	Model       string                    `json:"model"`
	MaxTokens   int                       `json:"max_tokens"`
	System      json.RawMessage           `json:"system,omitempty"`
	Messages    []anthropicInboundMessage `json:"messages"`
	Temperature float64                   `json:"temperature,omitempty"`
	Metadata    map[string]string         `json:"metadata,omitempty"`
}

type anthropicInboundMessage struct {
	Role    string          `json:"role"`
	Content json.RawMessage `json:"content"`
}

type anthropicInboundTextBlock struct {
	Type string `json:"type"`
	Text string `json:"text,omitempty"`
}

type anthropicMessageResponse struct {
	ID           string                        `json:"id"`
	Type         string                        `json:"type"`
	Role         string                        `json:"role"`
	Content      []anthropicInboundTextBlock   `json:"content"`
	Model        string                        `json:"model"`
	StopReason   string                        `json:"stop_reason"`
	StopSequence *string                       `json:"stop_sequence"`
	Usage        anthropicMessageResponseUsage `json:"usage"`
}

type anthropicMessageResponseUsage struct {
	InputTokens              int `json:"input_tokens"`
	OutputTokens             int `json:"output_tokens"`
	CacheCreationInputTokens int `json:"cache_creation_input_tokens"`
	CacheReadInputTokens     int `json:"cache_read_input_tokens"`
}

type anthropicErrorResponse struct {
	Type  string              `json:"type"`
	Error anthropicErrorShape `json:"error"`
}

type anthropicErrorShape struct {
	Type    string `json:"type"`
	Message string `json:"message"`
}

func isAnthropicMessagesPath(path string) bool {
	return path == anthropicMessagesPath
}

func decodeAnthropicRequest(body []byte) (LLMRequest, error) {
	var raw anthropicInboundRequest
	if err := json.Unmarshal(body, &raw); err != nil {
		return LLMRequest{}, err
	}

	systemBlocks, err := decodeAnthropicSystem(raw.System)
	if err != nil {
		return LLMRequest{}, fmt.Errorf("decode anthropic system blocks: %w", err)
	}

	messages := make([]Message, 0, len(raw.Messages))
	for _, msg := range raw.Messages {
		content, err := decodeAnthropicContent(msg.Content)
		if err != nil {
			return LLMRequest{}, fmt.Errorf("decode anthropic message content: %w", err)
		}
		messages = append(messages, Message{
			Role:    msg.Role,
			Content: content,
		})
	}

	return LLMRequest{
		Messages:          messages,
		SystemBlocks:      systemBlocks,
		Temperature:       raw.Temperature,
		MaxTokens:         raw.MaxTokens,
		Model:             raw.Model,
		Metadata:          raw.Metadata,
		Format:            RequestFormatAnthropic,
		PreferredProvider: "anthropic-direct",
	}, nil
}

func decodeAnthropicSystem(raw json.RawMessage) ([]SystemBlock, error) {
	if len(raw) == 0 || string(raw) == "null" {
		return nil, nil
	}

	var asString string
	if err := json.Unmarshal(raw, &asString); err == nil {
		if strings.TrimSpace(asString) == "" {
			return nil, nil
		}
		return []SystemBlock{{Type: "text", Text: asString}}, nil
	}

	var blocks []SystemBlock
	if err := json.Unmarshal(raw, &blocks); err == nil {
		for i := range blocks {
			if blocks[i].Type == "" {
				blocks[i].Type = "text"
			}
		}
		return blocks, nil
	}

	var textBlocks []anthropicInboundTextBlock
	if err := json.Unmarshal(raw, &textBlocks); err == nil {
		blocks = make([]SystemBlock, 0, len(textBlocks))
		for _, block := range textBlocks {
			if block.Type != "" && block.Type != "text" {
				continue
			}
			blocks = append(blocks, SystemBlock{
				Type: "text",
				Text: block.Text,
			})
		}
		return blocks, nil
	}

	return nil, fmt.Errorf("unsupported anthropic system payload")
}

func decodeAnthropicContent(raw json.RawMessage) (string, error) {
	if len(raw) == 0 || string(raw) == "null" {
		return "", nil
	}

	var asString string
	if err := json.Unmarshal(raw, &asString); err == nil {
		return asString, nil
	}

	var blocks []anthropicInboundTextBlock
	if err := json.Unmarshal(raw, &blocks); err == nil {
		var b strings.Builder
		for _, block := range blocks {
			if block.Type != "" && block.Type != "text" {
				continue
			}
			b.WriteString(block.Text)
		}
		return b.String(), nil
	}

	return "", fmt.Errorf("unsupported anthropic content payload")
}

func extractAnthropicPassthroughHeaders(header http.Header) map[string]string {
	allowed := []string{"Authorization", "X-API-Key", "Anthropic-Version", "Anthropic-Beta"}
	out := make(map[string]string, len(allowed))
	for _, key := range allowed {
		if value := strings.TrimSpace(header.Get(key)); value != "" {
			out[strings.ToLower(key)] = value
		}
	}
	if len(out) == 0 {
		return nil
	}
	return out
}

func clonePassthroughHeaders(in map[string]string) map[string]string {
	if len(in) == 0 {
		return nil
	}
	out := make(map[string]string, len(in))
	for k, v := range in {
		out[k] = v
	}
	return out
}

func buildAnthropicMessageResponse(resp PipelineResponse) anthropicMessageResponse {
	id := "msg_" + strings.ReplaceAll(resp.RequestID, "-", "")
	if resp.RequestID == "" {
		id = "msg_gateway"
	}
	stopReason := resp.FinishReason
	if stopReason == "" {
		stopReason = "end_turn"
	}

	return anthropicMessageResponse{
		ID:   id,
		Type: "message",
		Role: "assistant",
		Content: []anthropicInboundTextBlock{
			{
				Type: "text",
				Text: resp.Content,
			},
		},
		Model:        resp.Model,
		StopReason:   stopReason,
		StopSequence: nil,
		Usage: anthropicMessageResponseUsage{
			InputTokens:              resp.InputTokens,
			OutputTokens:             resp.OutputTokens,
			CacheCreationInputTokens: 0,
			CacheReadInputTokens:     0,
		},
	}
}

func writeAnthropicError(w http.ResponseWriter, status int, message string) {
	errorType := "api_error"
	switch status {
	case http.StatusBadRequest:
		errorType = "invalid_request_error"
	case http.StatusUnauthorized, http.StatusForbidden:
		errorType = "authentication_error"
	case http.StatusTooManyRequests:
		errorType = "rate_limit_error"
	case http.StatusUnprocessableEntity:
		errorType = "invalid_request_error"
	}

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(anthropicErrorResponse{
		Type: "error",
		Error: anthropicErrorShape{
			Type:    errorType,
			Message: message,
		},
	})
}
