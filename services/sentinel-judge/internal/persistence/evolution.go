package persistence

import (
	"database/sql"
	"fmt"
	"sync"
	"time"

	_ "modernc.org/sqlite"
)

const (
	maxRowsPerAgentField      = 2000
	globalRetentionCheckEvery = 100
	globalRetentionHighWater  = 499000
	globalRetentionRetainRows = 490000
	maxPlausibleSimTickExcl   = int64(1_000_000_000)
)

// EvolutionEntry represents a single personality evolution record.
type EvolutionEntry struct {
	AgentID    string
	Tick       int64
	Field      string
	ChangeType string // "drift", "quality", "fatigue", "voice_style"
	OldValue   string
	NewValue   string
	Reason     string
	NMDAScore  *float64
	Source     string // "realtime_judge", "batch_judge"
}

// EvolutionStore manages personality_evolution persistence.
type EvolutionStore struct {
	db                         *sql.DB
	mu                         sync.Mutex
	writesSinceGlobalRetention int
}

// OpenEvolution opens or creates the evolution database.
func OpenEvolution(path string) (*EvolutionStore, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("evolution open: %w", err)
	}
	db.SetMaxOpenConns(1)

	if _, err := db.Exec("PRAGMA journal_mode = WAL"); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("evolution pragma: %w", err)
	}
	if _, err := db.Exec(createEvolutionTable); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("evolution create table: %w", err)
	}
	if _, err := db.Exec(createEvolutionIndices); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("evolution create indices: %w", err)
	}
	return &EvolutionStore{db: db}, nil
}

// Write inserts a personality evolution entry.
func (s *EvolutionStore) Write(entry EvolutionEntry) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	now := time.Now().UnixMilli()
	source := entry.Source
	if source == "" {
		source = "realtime_judge"
	}

	tx, err := s.db.Begin()
	if err != nil {
		return fmt.Errorf("evolution begin: %w", err)
	}
	defer func() { _ = tx.Rollback() }()

	if _, err := tx.Exec(`INSERT INTO personality_evolution
			(agent_id, tick, field, change_type, old_value, new_value, reason, nmda_score, source, created_at_ms)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		entry.AgentID, entry.Tick, entry.Field, entry.ChangeType,
		entry.OldValue, entry.NewValue, entry.Reason, entry.NMDAScore,
		source, now,
	); err != nil {
		return fmt.Errorf("evolution write: %w", err)
	}

	if err := s.enforceAgentFieldRetentionTx(tx, entry.AgentID, entry.Field, maxRowsPerAgentField); err != nil {
		return err
	}

	s.writesSinceGlobalRetention++
	if s.writesSinceGlobalRetention >= globalRetentionCheckEvery {
		if err := s.enforceGlobalRetentionTx(tx, globalRetentionHighWater, globalRetentionRetainRows); err != nil {
			return err
		}
		s.writesSinceGlobalRetention = 0
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("evolution commit: %w", err)
	}
	return nil
}

func (s *EvolutionStore) enforceAgentFieldRetentionTx(tx *sql.Tx, agentID, field string, keep int) error {
	_, err := tx.Exec(`DELETE FROM personality_evolution
		WHERE agent_id = ?
		  AND field = ?
		  AND id <= (
			SELECT id
			FROM personality_evolution
			WHERE agent_id = ? AND field = ?
			ORDER BY id DESC
			LIMIT 1 OFFSET ?
		  )`,
		agentID, field, agentID, field, keep,
	)
	if err != nil {
		return fmt.Errorf("evolution agent-field retention: %w", err)
	}
	return nil
}

func (s *EvolutionStore) enforceGlobalRetentionTx(tx *sql.Tx, highWater, retainRows int) error {
	var count int
	if err := tx.QueryRow("SELECT COUNT(*) FROM personality_evolution").Scan(&count); err != nil {
		return fmt.Errorf("evolution global retention count: %w", err)
	}
	if count <= highWater {
		return nil
	}
	_, err := tx.Exec(`DELETE FROM personality_evolution
		WHERE id <= (
			SELECT id
			FROM personality_evolution
			ORDER BY id DESC
			LIMIT 1 OFFSET ?
		)`, retainRows)
	if err != nil {
		return fmt.Errorf("evolution global retention: %w", err)
	}
	return nil
}

// DeleteInvalidTicks removes legacy rows whose tick is not a plausible simulation tick.
// It is intended for offline migration/compaction before the judge is restarted.
func (s *EvolutionStore) DeleteInvalidTicks() (int64, error) {
	res, err := s.db.Exec("DELETE FROM personality_evolution WHERE tick < 0 OR tick >= ?", maxPlausibleSimTickExcl)
	if err != nil {
		return 0, fmt.Errorf("evolution delete invalid ticks: %w", err)
	}
	return res.RowsAffected()
}

// GetByAgent returns all evolution entries for an agent, ordered by tick.
func (s *EvolutionStore) GetByAgent(agentID string) ([]EvolutionEntry, error) {
	rows, err := s.db.Query(`SELECT agent_id, tick, field, change_type, old_value,
		new_value, reason, nmda_score, source
		FROM personality_evolution WHERE agent_id = ? ORDER BY tick ASC`, agentID)
	if err != nil {
		return nil, fmt.Errorf("evolution get by agent: %w", err)
	}
	defer func() { _ = rows.Close() }()

	var entries []EvolutionEntry
	for rows.Next() {
		var e EvolutionEntry
		var oldVal sql.NullString
		var nmda sql.NullFloat64
		if err := rows.Scan(&e.AgentID, &e.Tick, &e.Field, &e.ChangeType,
			&oldVal, &e.NewValue, &e.Reason, &nmda, &e.Source); err != nil {
			return nil, fmt.Errorf("evolution scan: %w", err)
		}
		if oldVal.Valid {
			e.OldValue = oldVal.String
		}
		if nmda.Valid {
			e.NMDAScore = &nmda.Float64
		}
		entries = append(entries, e)
	}
	return entries, rows.Err()
}

// Count returns the total number of evolution entries.
func (s *EvolutionStore) Count() (int64, error) {
	var n int64
	err := s.db.QueryRow("SELECT COUNT(*) FROM personality_evolution").Scan(&n)
	return n, err
}

// Close closes the database.
func (s *EvolutionStore) Close() error {
	return s.db.Close()
}
