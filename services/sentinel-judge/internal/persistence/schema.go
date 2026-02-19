// Package persistence manages the personality_evolution SQLite table
// used by the sentinel-judge to track agent personality changes over time.
package persistence

const createEvolutionTable = `CREATE TABLE IF NOT EXISTS personality_evolution (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	agent_id TEXT NOT NULL,
	tick INTEGER NOT NULL,
	field TEXT NOT NULL,
	change_type TEXT NOT NULL,
	old_value TEXT,
	new_value TEXT NOT NULL,
	reason TEXT NOT NULL,
	nmda_score REAL,
	source TEXT NOT NULL DEFAULT 'realtime_judge',
	created_at_ms INTEGER NOT NULL
)`

const createEvolutionIndices = `
CREATE INDEX IF NOT EXISTS idx_evolution_agent ON personality_evolution(agent_id, tick);
CREATE INDEX IF NOT EXISTS idx_evolution_source ON personality_evolution(source)
`
