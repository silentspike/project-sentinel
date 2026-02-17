package observatory

import (
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strconv"
	"time"
)

const maxBodySize = 4 * 1024 * 1024 // 4 MB

// Handler serves HTTP endpoints for the MARBLE Observatory.
type Handler struct {
	store  *SqliteStore
	config *ObservatoryConfig
	logger *slog.Logger
}

// NewHandler creates an observatory HTTP handler.
// If logger is nil, a no-op logger is used.
func NewHandler(store *SqliteStore, config *ObservatoryConfig, logger *slog.Logger) *Handler {
	if logger == nil {
		logger = slog.New(slog.NewTextHandler(io.Discard, nil))
	}
	return &Handler{store: store, config: config, logger: logger}
}

// RegisterRoutes adds observatory endpoints to the given ServeMux.
func (h *Handler) RegisterRoutes(mux *http.ServeMux) {
	mux.HandleFunc("POST /observatory/runs", h.handleSubmitRun)
	mux.HandleFunc("GET /observatory/runs", h.handleListRuns)
	mux.HandleFunc("GET /observatory/runs/{run_id}", h.handleGetRun)
	mux.HandleFunc("GET /observatory/report", h.handleReport)
}

// submitRunRequest is the JSON body for POST /observatory/runs.
type submitRunRequest struct {
	Records []submitRecord `json:"records"`
}

type submitRecord struct {
	Timestamp string          `json:"timestamp"` // RFC3339
	Shift     int             `json:"shift"`
	Model     string          `json:"model"`
	Agent     string          `json:"agent"`
	Scenario  string          `json:"scenario"`
	Metrics   MetricsSnapshot `json:"metrics"`
}

type submitRunResponse struct {
	RunID         string `json:"run_id"`
	ConfigHash    string `json:"config_hash"`
	RecordsStored int    `json:"records_stored"`
	Status        string `json:"status"`
}

func (h *Handler) handleSubmitRun(w http.ResponseWriter, r *http.Request) {
	defer func() { _ = r.Body.Close() }()

	body, err := io.ReadAll(io.LimitReader(r.Body, maxBodySize+1))
	if err != nil {
		http.Error(w, "failed to read request body", http.StatusBadRequest)
		return
	}
	if len(body) > maxBodySize {
		http.Error(w, "request body too large", http.StatusRequestEntityTooLarge)
		return
	}

	var req submitRunRequest
	if err := json.Unmarshal(body, &req); err != nil {
		http.Error(w, "invalid JSON body", http.StatusBadRequest)
		return
	}

	if len(req.Records) == 0 {
		http.Error(w, "records array must not be empty", http.StatusBadRequest)
		return
	}

	// Convert to ObservationRecords
	records := make([]ObservationRecord, 0, len(req.Records))
	for i, sr := range req.Records {
		ts, err := time.Parse(time.RFC3339, sr.Timestamp)
		if err != nil {
			ts = time.Now()
		}
		if sr.Shift < 1 || sr.Shift > 3 {
			http.Error(w, fmt.Sprintf("record[%d]: shift must be 1-3, got %d", i, sr.Shift), http.StatusBadRequest)
			return
		}
		if sr.Model == "" {
			http.Error(w, fmt.Sprintf("record[%d]: model must not be empty", i), http.StatusBadRequest)
			return
		}
		records = append(records, ObservationRecord{
			Timestamp: ts,
			Shift:     sr.Shift,
			Model:     sr.Model,
			Agent:     sr.Agent,
			Scenario:  sr.Scenario,
			Metrics:   sr.Metrics,
		})
	}

	runID := generateUUID()
	configHash := ConfigHash(h.config)

	if err := h.store.SubmitRun(runID, configHash, records); err != nil {
		h.logger.Error("observatory submit run failed", "error", err)
		http.Error(w, "failed to store run", http.StatusInternalServerError)
		return
	}

	h.logger.Info("observatory run submitted", "run_id", runID, "records", len(records))

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusCreated)
	_ = json.NewEncoder(w).Encode(submitRunResponse{
		RunID:         runID,
		ConfigHash:    configHash,
		RecordsStored: len(records),
		Status:        "completed",
	})
}

type listRunsResponse struct {
	Runs []RunSummary `json:"runs"`
}

func (h *Handler) handleListRuns(w http.ResponseWriter, _ *http.Request) {
	runs, err := h.store.ListRuns()
	if err != nil {
		h.logger.Error("observatory list runs failed", "error", err)
		http.Error(w, "failed to list runs", http.StatusInternalServerError)
		return
	}
	if runs == nil {
		runs = []RunSummary{}
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(listRunsResponse{Runs: runs})
}

type getRunResponse struct {
	RunID       string              `json:"run_id"`
	Records     []ObservationRecord `json:"records"`
	RecordCount int                 `json:"record_count"`
}

func (h *Handler) handleGetRun(w http.ResponseWriter, r *http.Request) {
	runID := r.PathValue("run_id")
	if runID == "" {
		http.Error(w, "run_id is required", http.StatusBadRequest)
		return
	}

	filter := parseQueryFilter(r)

	records, err := h.store.GetRunRecords(runID, filter)
	if err != nil {
		h.logger.Error("observatory get run failed", "error", err)
		http.Error(w, "failed to get run records", http.StatusInternalServerError)
		return
	}
	if records == nil {
		records = []ObservationRecord{}
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(getRunResponse{
		RunID:       runID,
		Records:     records,
		RecordCount: len(records),
	})
}

func (h *Handler) handleReport(w http.ResponseWriter, r *http.Request) {
	format := r.URL.Query().Get("format")
	if format == "" {
		format = "json"
	}

	filter := parseQueryFilter(r)
	report := NewReportGenerator(h.store)

	switch format {
	case "json":
		data, err := report.GenerateJSON(filter)
		if err != nil {
			h.logger.Error("observatory generate json report failed", "error", err)
			http.Error(w, "failed to generate report", http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write(data)

	case "markdown":
		md := report.GenerateMarkdown(filter)
		w.Header().Set("Content-Type", "text/markdown; charset=utf-8")
		_, _ = fmt.Fprint(w, md)

	default:
		http.Error(w, "format must be 'json' or 'markdown'", http.StatusBadRequest)
	}
}

// parseQueryFilter extracts optional shift/model/scenario filter params from the request.
func parseQueryFilter(r *http.Request) QueryFilter {
	var filter QueryFilter

	if s := r.URL.Query().Get("shift"); s != "" {
		if v, err := strconv.Atoi(s); err == nil {
			filter.Shift = &v
		}
	}
	if m := r.URL.Query().Get("model"); m != "" {
		filter.Model = &m
	}
	if sc := r.URL.Query().Get("scenario"); sc != "" {
		filter.Scenario = &sc
	}

	return filter
}
