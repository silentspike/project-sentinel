package observatory

import (
	"database/sql"
	"fmt"
	"sync"
	"time"

	_ "modernc.org/sqlite"
)

const sqlitePragmas = `
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA mmap_size = 268435456;
PRAGMA page_size = 8192
`

const createRuns = `CREATE TABLE IF NOT EXISTS runs (
	id          INTEGER PRIMARY KEY AUTOINCREMENT,
	run_id      TEXT NOT NULL UNIQUE,
	started_at  INTEGER NOT NULL,
	finished_at INTEGER,
	config_hash TEXT NOT NULL,
	status      TEXT NOT NULL DEFAULT 'completed'
)`

const createRunsIndex = `CREATE INDEX IF NOT EXISTS idx_runs_status ON runs(status)`

const createObservations = `CREATE TABLE IF NOT EXISTS observations (
	id                      INTEGER PRIMARY KEY AUTOINCREMENT,
	run_id                  TEXT NOT NULL REFERENCES runs(run_id),
	timestamp_ms            INTEGER NOT NULL,
	shift                   INTEGER NOT NULL CHECK (shift BETWEEN 1 AND 3),
	model                   TEXT NOT NULL,
	agent                   TEXT NOT NULL,
	scenario                TEXT NOT NULL,
	info_propagation        REAL NOT NULL,
	group_polarization      REAL NOT NULL,
	communication_score     REAL NOT NULL,
	personality_consistency REAL NOT NULL,
	response_creativity     REAL NOT NULL,
	emotional_range         REAL NOT NULL
)`

const createObsIndices = `
CREATE INDEX IF NOT EXISTS idx_obs_run ON observations(run_id);
CREATE INDEX IF NOT EXISTS idx_obs_shift ON observations(shift);
CREATE INDEX IF NOT EXISTS idx_obs_model ON observations(model)
`

// SqliteStore provides persistent storage for observation records backed by SQLite.
// It keeps an in-memory cache for fast reads and writes through to SQLite.
type SqliteStore struct {
	db      *sql.DB
	mu      sync.RWMutex
	records []ObservationRecord
}

// OpenSqliteStore opens (or creates) a SQLite observatory database at path.
// It runs migrations and loads all existing records into the memory cache (AC-4).
func OpenSqliteStore(path string) (*SqliteStore, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("observatory open: %w", err)
	}

	db.SetMaxOpenConns(1)

	for _, ddl := range []string{sqlitePragmas, createRuns, createRunsIndex, createObservations, createObsIndices} {
		if _, err := db.Exec(ddl); err != nil {
			_ = db.Close()
			return nil, fmt.Errorf("observatory migrate: %w", err)
		}
	}

	s := &SqliteStore{db: db}
	if err := s.loadRecords(); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("observatory load records: %w", err)
	}

	return s, nil
}

// loadRecords reads all observation rows from SQLite into the memory cache.
func (s *SqliteStore) loadRecords() error {
	rows, err := s.db.Query(`SELECT timestamp_ms, shift, model, agent, scenario,
		info_propagation, group_polarization, communication_score,
		personality_consistency, response_creativity, emotional_range
		FROM observations ORDER BY id`)
	if err != nil {
		return err
	}
	defer rows.Close()

	var records []ObservationRecord
	for rows.Next() {
		var r ObservationRecord
		var tsMs int64
		if err := rows.Scan(&tsMs, &r.Shift, &r.Model, &r.Agent, &r.Scenario,
			&r.Metrics.InfoPropagation, &r.Metrics.GroupPolarization,
			&r.Metrics.CommunicationScore, &r.Metrics.PersonalityConsistency,
			&r.Metrics.ResponseCreativity, &r.Metrics.EmotionalRange,
		); err != nil {
			return err
		}
		r.Timestamp = time.UnixMilli(tsMs)
		records = append(records, r)
	}
	if err := rows.Err(); err != nil {
		return err
	}

	s.records = records
	return nil
}

// Add appends an observation record to both SQLite and the memory cache.
func (s *SqliteStore) Add(record ObservationRecord) {
	s.mu.Lock()
	defer s.mu.Unlock()

	_, _ = s.db.Exec(`INSERT INTO observations
		(run_id, timestamp_ms, shift, model, agent, scenario,
		 info_propagation, group_polarization, communication_score,
		 personality_consistency, response_creativity, emotional_range)
		VALUES ('_unassigned', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		record.Timestamp.UnixMilli(), record.Shift, record.Model,
		record.Agent, record.Scenario,
		record.Metrics.InfoPropagation, record.Metrics.GroupPolarization,
		record.Metrics.CommunicationScore, record.Metrics.PersonalityConsistency,
		record.Metrics.ResponseCreativity, record.Metrics.EmotionalRange,
	)

	s.records = append(s.records, record)
}

// Query returns all records matching the given filter criteria (from memory cache).
func (s *SqliteStore) Query(filter QueryFilter) []ObservationRecord {
	s.mu.RLock()
	defer s.mu.RUnlock()

	var results []ObservationRecord
	for _, r := range s.records {
		if matchesFilter(r, filter) {
			results = append(results, r)
		}
	}
	return results
}

// AllRecords returns a copy of all stored observation records.
func (s *SqliteStore) AllRecords() []ObservationRecord {
	s.mu.RLock()
	defer s.mu.RUnlock()

	result := make([]ObservationRecord, len(s.records))
	copy(result, s.records)
	return result
}

// Len returns the number of stored records.
func (s *SqliteStore) Len() int {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return len(s.records)
}

// SubmitRun atomically inserts a run and its observation records in a single transaction.
func (s *SqliteStore) SubmitRun(runID, configHash string, records []ObservationRecord) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	tx, err := s.db.Begin()
	if err != nil {
		return fmt.Errorf("observatory begin tx: %w", err)
	}
	defer func() { _ = tx.Rollback() }()

	now := time.Now().UnixMilli()
	if _, err := tx.Exec(`INSERT INTO runs (run_id, started_at, finished_at, config_hash, status)
		VALUES (?, ?, ?, ?, 'completed')`, runID, now, now, configHash); err != nil {
		return fmt.Errorf("observatory insert run: %w", err)
	}

	stmt, err := tx.Prepare(`INSERT INTO observations
		(run_id, timestamp_ms, shift, model, agent, scenario,
		 info_propagation, group_polarization, communication_score,
		 personality_consistency, response_creativity, emotional_range)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`)
	if err != nil {
		return fmt.Errorf("observatory prepare obs: %w", err)
	}
	defer stmt.Close()

	for _, r := range records {
		if _, err := stmt.Exec(runID, r.Timestamp.UnixMilli(), r.Shift, r.Model,
			r.Agent, r.Scenario,
			r.Metrics.InfoPropagation, r.Metrics.GroupPolarization,
			r.Metrics.CommunicationScore, r.Metrics.PersonalityConsistency,
			r.Metrics.ResponseCreativity, r.Metrics.EmotionalRange,
		); err != nil {
			return fmt.Errorf("observatory insert observation: %w", err)
		}
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("observatory commit: %w", err)
	}

	s.records = append(s.records, records...)
	return nil
}

// ListRuns returns metadata for all benchmark runs.
func (s *SqliteStore) ListRuns() ([]RunSummary, error) {
	rows, err := s.db.Query(`SELECT r.run_id, r.started_at, r.finished_at, r.config_hash, r.status,
		COUNT(o.id) as record_count
		FROM runs r LEFT JOIN observations o ON r.run_id = o.run_id
		GROUP BY r.run_id ORDER BY r.started_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var runs []RunSummary
	for rows.Next() {
		var rs RunSummary
		var startMs int64
		var finishMs sql.NullInt64
		if err := rows.Scan(&rs.RunID, &startMs, &finishMs, &rs.ConfigHash, &rs.Status, &rs.RecordCount); err != nil {
			return nil, err
		}
		rs.StartedAt = time.UnixMilli(startMs)
		if finishMs.Valid {
			rs.FinishedAt = time.UnixMilli(finishMs.Int64)
		}
		runs = append(runs, rs)
	}
	return runs, rows.Err()
}

// GetRunRecords returns observation records for a specific run, optionally filtered.
func (s *SqliteStore) GetRunRecords(runID string, filter QueryFilter) ([]ObservationRecord, error) {
	rows, err := s.db.Query(`SELECT timestamp_ms, shift, model, agent, scenario,
		info_propagation, group_polarization, communication_score,
		personality_consistency, response_creativity, emotional_range
		FROM observations WHERE run_id = ? ORDER BY id`, runID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var records []ObservationRecord
	for rows.Next() {
		var r ObservationRecord
		var tsMs int64
		if err := rows.Scan(&tsMs, &r.Shift, &r.Model, &r.Agent, &r.Scenario,
			&r.Metrics.InfoPropagation, &r.Metrics.GroupPolarization,
			&r.Metrics.CommunicationScore, &r.Metrics.PersonalityConsistency,
			&r.Metrics.ResponseCreativity, &r.Metrics.EmotionalRange,
		); err != nil {
			return nil, err
		}
		r.Timestamp = time.UnixMilli(tsMs)
		if matchesFilter(r, filter) {
			records = append(records, r)
		}
	}
	return records, rows.Err()
}

// Close closes the underlying database connection.
func (s *SqliteStore) Close() error {
	return s.db.Close()
}
