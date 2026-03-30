package synthesis

import (
	"fmt"
	"strconv"
	"strings"
)

// Fingerprint holds parsed synthesis fingerprint data from the daemon.
// Format: "H{0-9}|E{0-9}|B{0-9}|S{0-9}|C{0-9}|SN{0-9}|R:{room}|P:{n}|CH:{0|1}|HR:{0|1}|T:{hour}|TMP:{0|1}|PE:{I|E}|IM:{0|1}"
type Fingerprint struct {
	Hunger        int    // 0-9 bucket (0=low, 9=high)
	Energy        int    // 0-9 bucket
	Bladder       int    // 0-9 bucket
	Stress        int    // 0-9 bucket
	Caffeine      int    // 0-9 bucket
	Social        int    // 0-9 bucket
	RoomID        string // e.g. "buero-dev-1"
	PresenceCount int    // number of other agents in room
	HasChaos      bool   // active chaos event
	HasHeard      bool   // heard_text non-empty (chat in room)
	SimHour       int    // 0-23, simulation hour
	TempHigh      bool   // room temperature > 26°C
	Personality   string // "I" (introvert) or "E" (extrovert)
	HasImpulse    bool   // Operator-Impulse active (Gaia/Broadcast)
}

// Parse parses a fingerprint string from daemon metadata.
//
//nolint:gocyclo // compact field parser intentionally handles all fingerprint tags in one pass
func Parse(raw string) (Fingerprint, error) {
	if raw == "" {
		return Fingerprint{}, fmt.Errorf("empty fingerprint")
	}

	var fp Fingerprint
	fields := strings.Split(raw, "|")

	for _, field := range fields {
		key, val, hasColon := strings.Cut(field, ":")

		switch key {
		case "R":
			if !hasColon {
				return fp, fmt.Errorf("r field missing value")
			}
			fp.RoomID = val
		case "P":
			if !hasColon {
				return fp, fmt.Errorf("p field missing value")
			}
			n, err := strconv.Atoi(val)
			if err != nil {
				return fp, fmt.Errorf("p field: %w", err)
			}
			fp.PresenceCount = n
		case "CH":
			if !hasColon {
				return fp, fmt.Errorf("CH field missing value")
			}
			fp.HasChaos = val == "1"
		case "HR":
			if !hasColon {
				return fp, fmt.Errorf("HR field missing value")
			}
			fp.HasHeard = val == "1"
		case "T":
			if !hasColon {
				return fp, fmt.Errorf("t field missing value")
			}
			n, err := strconv.Atoi(val)
			if err != nil {
				return fp, fmt.Errorf("t field: %w", err)
			}
			fp.SimHour = n
		case "TMP":
			if !hasColon {
				return fp, fmt.Errorf("TMP field missing value")
			}
			fp.TempHigh = val == "1"
		case "PE":
			if !hasColon {
				return fp, fmt.Errorf("PE field missing value")
			}
			fp.Personality = val
		case "IM":
			if !hasColon {
				return fp, fmt.Errorf("IM field missing value")
			}
			fp.HasImpulse = val == "1"
		default:
			// Bio buckets: single-letter prefix + digit, e.g. "H5", "E7", "SN3"
			if err := parseBioBucket(field, &fp); err != nil {
				return fp, err
			}
		}
	}

	return fp, nil
}

// parseBioBucket parses bio bucket fields like "H5", "E7", "SN3".
func parseBioBucket(field string, fp *Fingerprint) error {
	// Try two-char prefix first (SN)
	if strings.HasPrefix(field, "SN") {
		n, err := strconv.Atoi(field[2:])
		if err != nil {
			return fmt.Errorf("SN field: %w", err)
		}
		fp.Social = n
		return nil
	}

	// Single-char prefix: H, E, B, S, C
	if len(field) < 2 {
		return fmt.Errorf("unknown fingerprint field: %q", field)
	}

	n, err := strconv.Atoi(field[1:])
	if err != nil {
		return fmt.Errorf("bio bucket %q: %w", field, err)
	}

	switch field[0] {
	case 'H':
		fp.Hunger = n
	case 'E':
		fp.Energy = n
	case 'B':
		fp.Bladder = n
	case 'S':
		fp.Stress = n
	case 'C':
		fp.Caffeine = n
	default:
		return fmt.Errorf("unknown fingerprint field: %q", field)
	}

	return nil
}
