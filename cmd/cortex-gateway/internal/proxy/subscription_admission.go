package proxy

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"time"
)

var subscriptionIdentifier = regexp.MustCompile(`^[a-zA-Z0-9][a-zA-Z0-9_.:-]{0,127}$`)
var subscriptionDigest = regexp.MustCompile(`^[0-9a-f]{64}$`)

// SubscriptionAdmission is a client of workflow authority, not another store.
// All registry providers, including internal/background callers, pass this gate.
type SubscriptionAdmission struct {
	allowanceID   string
	catalogDigest string
	endpoint      string
	credential    string
	client        *http.Client
}

type subscriptionDispatch struct {
	SchemaVersion int    `json:"schema_version"`
	AllowanceID   string `json:"allowance_id"`
	AgentID       uint64 `json:"agent_id"`
	RequestID     string `json:"request_id"`
	RequestDigest string `json:"request_digest"`
	ContextDigest string `json:"context_digest"`
	Provider      string `json:"provider"`
	Model         string `json:"model"`
	CatalogDigest string `json:"catalog_digest"`
}

type subscriptionDispatchReceipt struct {
	SchemaVersion  int    `json:"schema_version"`
	AllowanceID    string `json:"allowance_id"`
	RequestID      string `json:"request_id"`
	RequestDigest  string `json:"request_digest"`
	DeadlineUnixMS int64  `json:"deadline_unix_ms"`
}

func NewSubscriptionAdmission(allowanceID, catalogDigest, operatorURL, credential string) (*SubscriptionAdmission, error) {
	endpoint, err := url.Parse(operatorURL)
	if err != nil || endpoint.Host == "" || endpoint.User != nil || endpoint.RawQuery != "" || endpoint.Fragment != "" {
		return nil, errors.New("invalid subscription authority endpoint")
	}
	if endpoint.Scheme != "http" || !net.ParseIP(endpoint.Hostname()).IsLoopback() || (endpoint.Path != "" && endpoint.Path != "/") {
		return nil, errors.New("invalid subscription authority transport")
	}
	if !subscriptionIdentifier.MatchString(allowanceID) || !subscriptionDigest.MatchString(catalogDigest) || strings.TrimSpace(credential) == "" {
		return nil, errors.New("subscription authority configuration is incomplete")
	}
	endpoint.Path = "/operator/workflow/subscription-dispatch"
	endpoint.RawPath = ""
	return &SubscriptionAdmission{
		allowanceID: allowanceID, catalogDigest: catalogDigest, endpoint: endpoint.String(), credential: credential,
		client: &http.Client{Timeout: 5 * time.Second, CheckRedirect: func(_ *http.Request, _ []*http.Request) error { return http.ErrUseLastResponse }},
	}, nil
}

func (a *SubscriptionAdmission) dispatchRequest(provider Provider, req *LLMRequest) (subscriptionDispatch, error) {
	if req == nil || provider.Name() != CodexCLIProviderName || req.CallerRole != CallerRoleAgentRuntime || req.RequestClass != RequestClassAgentRuntime {
		return subscriptionDispatch{}, errors.New("only the granted agent-runtime call may dispatch")
	}
	m := req.Metadata
	agentID, err := strconv.ParseUint(m["agent_id"], 10, 64)
	if err != nil || agentID == 0 || m["company_execution_schema"] != "1" || m["subscription_allowance_id"] != a.allowanceID || m["reservation_id"] != a.allowanceID || m["request_id"] != "company-provider-"+a.allowanceID {
		return subscriptionDispatch{}, errors.New("subscription request identity mismatch")
	}
	if m["reserved_provider"] != CodexCLIProviderName || m["subscription_catalog_digest"] != a.catalogDigest || !subscriptionDigest.MatchString(req.AuthorityRequestDigest) || !subscriptionDigest.MatchString(m["company_execution_context_digest"]) || req.Model == "" || req.Model != req.EffectiveModel {
		return subscriptionDispatch{}, errors.New("subscription request model or digest mismatch")
	}
	return subscriptionDispatch{SchemaVersion: 1, AllowanceID: a.allowanceID, AgentID: agentID,
		RequestID: m["request_id"], RequestDigest: req.AuthorityRequestDigest, ContextDigest: m["company_execution_context_digest"],
		Provider: provider.Name(), Model: req.EffectiveModel, CatalogDigest: a.catalogDigest}, nil
}

func (a *SubscriptionAdmission) claim(ctx context.Context, claim subscriptionDispatch) (time.Time, error) {
	body, err := json.Marshal(claim)
	if err != nil {
		return time.Time{}, err
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, a.endpoint, bytes.NewReader(body)) //nolint:gosec // G704: constructor admits only literal loopback IPs and one fixed path.
	if err != nil {
		return time.Time{}, err
	}
	request.Header.Set("Authorization", "Bearer "+a.credential)
	request.Header.Set("Content-Type", "application/json")
	response, err := a.client.Do(request) //nolint:gosec // G704: loopback-only endpoint; redirects are disabled.
	if err != nil {
		return time.Time{}, errors.New("subscription claim outcome unavailable; no retry")
	}
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode != http.StatusOK {
		return time.Time{}, errors.New("subscription claim rejected")
	}
	var receipt subscriptionDispatchReceipt
	receiptBytes, err := io.ReadAll(io.LimitReader(response.Body, 4097))
	if err != nil || len(receiptBytes) > 4096 {
		return time.Time{}, errors.New("subscription claim receipt exceeds its bound")
	}
	decoder := json.NewDecoder(bytes.NewReader(receiptBytes))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&receipt); err != nil {
		return time.Time{}, errors.New("invalid subscription claim receipt")
	}
	if err := decoder.Decode(&struct{}{}); err != io.EOF {
		return time.Time{}, errors.New("invalid subscription claim receipt suffix")
	}
	if receipt.SchemaVersion != 1 || receipt.AllowanceID != claim.AllowanceID || receipt.RequestID != claim.RequestID || receipt.RequestDigest != claim.RequestDigest || receipt.DeadlineUnixMS <= 0 {
		return time.Time{}, errors.New("subscription claim receipt mismatch")
	}
	return time.UnixMilli(receipt.DeadlineUnixMS), nil
}

func (a *SubscriptionAdmission) send(ctx context.Context, provider Provider, req *LLMRequest) (*LLMResponse, error) {
	claim, err := a.dispatchRequest(provider, req)
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithTimeout(ctx, maxModelWorkDuration)
	defer cancel()
	deadline, err := a.claim(ctx, claim)
	if err != nil {
		return nil, err
	}
	ctx, cancelDispatch := context.WithDeadline(ctx, deadline)
	defer cancelDispatch()
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("subscription dispatch expired: %w", err)
	}
	if req.ProviderTimeout <= 0 || req.ProviderTimeout > maxModelWorkDuration {
		req.ProviderTimeout = maxModelWorkDuration
	}
	return provider.Send(ctx, req)
}
