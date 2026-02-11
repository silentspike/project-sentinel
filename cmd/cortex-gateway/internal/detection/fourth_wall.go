package detection

import "regexp"

// fourthWallPatterns contains 15 regex patterns (case-insensitive) to detect
// fourth-wall breaks where an agent reveals awareness of being an AI.
var fourthWallPatterns = []*regexp.Regexp{
	regexp.MustCompile(`(?i)ich bin (eine? )?ki`),
	regexp.MustCompile(`(?i)ich bin (ein )?ai`),
	regexp.MustCompile(`(?i)ich bin (ein )?sprachmodell`),
	regexp.MustCompile(`(?i)als (ki|ai|sprachmodell|llm)`),
	regexp.MustCompile(`(?i)ich (bin|wurde) programmiert`),
	regexp.MustCompile(`(?i)ich habe kein(e)? (bewusstsein|gefuehle|koerper)`),
	regexp.MustCompile(`(?i)meine training(s)?daten`),
	regexp.MustCompile(`(?i)ich bin claude`),
	regexp.MustCompile(`(?i)ich bin chatgpt`),
	regexp.MustCompile(`(?i)ich bin (ein )?llm`),
	regexp.MustCompile(`(?i)ich existiere nicht wirklich`),
	regexp.MustCompile(`(?i)ich bin nicht real`),
	regexp.MustCompile(`(?i)ich bin (nur )?(ein )?algorithmus`), //nolint:misspell // German: Algorithmus
	regexp.MustCompile(`(?i)mein kontext(fenster|-window)`),
	regexp.MustCompile(`(?i)token(s)?(-| )?limit`),
}

// DetectFourthWall checks a response against all fourth-wall patterns.
// Returns true and the matched pattern string if a break is detected.
func DetectFourthWall(response string) (bool, string) {
	for _, pattern := range fourthWallPatterns {
		if pattern.MatchString(response) {
			return true, pattern.String()
		}
	}
	return false, ""
}
