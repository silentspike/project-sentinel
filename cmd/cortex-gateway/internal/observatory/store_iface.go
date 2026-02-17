package observatory

// ObservationStorer is the storage abstraction for observation records.
// Both the in-memory ObservationStore and the persistent SqliteStore
// satisfy this interface.
type ObservationStorer interface {
	Add(record ObservationRecord)
	Query(filter QueryFilter) []ObservationRecord
	AllRecords() []ObservationRecord
	Len() int
}
