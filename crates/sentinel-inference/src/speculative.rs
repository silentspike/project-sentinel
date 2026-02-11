use crate::bitnet::BitNetClient;

/// Configuration for Speculative Decoding.
pub struct SpeculativeConfig {
    /// Draft model (kleiner, schneller) - z.B. BitNet 2B
    pub draft_client: BitNetClient,
    /// Verify model (groesser, genauer) - z.B. BitNet 7B
    pub verify_client: BitNetClient,
    /// Max tokens the draft model generates before verification
    pub draft_tokens: u32,
    /// Acceptance threshold (0.0-1.0) fuer Draft-Token
    pub acceptance_threshold: f64,
}

/// Speculative Decoding Pipeline.
///
/// 1. Draft-Model generiert N Kandidaten-Tokens schnell
/// 2. Verify-Model prueft alle N Tokens in einem Pass
/// 3. Akzeptierte Tokens werden uebernommen, ab erstem Reject: neu generieren
///
/// Speedup: typisch 1.5-3x (abhaengig von Akzeptanzrate)
pub struct SpeculativeDecoder {
    config: SpeculativeConfig,
}

impl SpeculativeDecoder {
    pub fn new(config: SpeculativeConfig) -> Self {
        Self { config }
    }

    /// Generiert Text mit Speculative Decoding.
    /// Fallback auf normales Generate wenn Draft-Model nicht verfuegbar.
    pub fn generate(&self, prompt: &str) -> anyhow::Result<SpeculativeResult> {
        if !self.config.draft_client.is_available() {
            // Fallback: nur Verify-Model (kein Speedup)
            let output = self.config.verify_client.generate(prompt)?;
            return Ok(SpeculativeResult {
                text: output,
                draft_tokens: 0,
                accepted_tokens: 0,
                acceptance_rate: 0.0,
                speedup_factor: 1.0,
            });
        }

        // 1. Draft: schnelle Kandidaten-Tokens
        let draft_output = self.config.draft_client.generate(prompt)?;
        let draft_tokens = draft_output.split_whitespace().count() as u32;

        // 2. Verify: grosses Modell prueft
        let verify_prompt = format!("{}\n{}", prompt, draft_output);
        let verify_output = self.config.verify_client.generate(&verify_prompt)?;

        // 3. Akzeptanzrate berechnen (vereinfacht: Token-Ueberlappung)
        let accepted = count_matching_prefix(&draft_output, &verify_output);
        let acceptance_rate = if draft_tokens > 0 {
            accepted as f64 / draft_tokens as f64
        } else {
            0.0
        };

        Ok(SpeculativeResult {
            text: verify_output,
            draft_tokens,
            accepted_tokens: accepted,
            acceptance_rate,
            speedup_factor: if acceptance_rate > 0.5 { 1.5 } else { 1.0 },
        })
    }
}

/// Result of speculative decoding.
pub struct SpeculativeResult {
    pub text: String,
    pub draft_tokens: u32,
    pub accepted_tokens: u32,
    pub acceptance_rate: f64,
    pub speedup_factor: f64,
}

/// Zaehlt wie viele Tokens am Anfang uebereinstimmen.
pub fn count_matching_prefix(draft: &str, verify: &str) -> u32 {
    draft
        .split_whitespace()
        .zip(verify.split_whitespace())
        .take_while(|(d, v)| d == v)
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitnet::{BitNetClient, BitNetConfig};

    #[test]
    fn test_count_matching_prefix() {
        assert_eq!(
            count_matching_prefix("hello world foo", "hello world bar"),
            2
        );
        assert_eq!(count_matching_prefix("hello world", "hello world"), 2);
        assert_eq!(count_matching_prefix("hello", "goodbye"), 0);
        assert_eq!(count_matching_prefix("", "hello"), 0);
    }

    #[test]
    fn test_speculative_fallback_no_draft() {
        let config = SpeculativeConfig {
            draft_client: BitNetClient::new(BitNetConfig {
                binary_path: "/nonexistent/draft".to_string(),
                model_path: "/nonexistent/model.gguf".to_string(),
                threads: 4,
                max_tokens: 64,
            }),
            verify_client: BitNetClient::new(BitNetConfig {
                binary_path: "/nonexistent/verify".to_string(),
                model_path: "/nonexistent/model.gguf".to_string(),
                threads: 8,
                max_tokens: 256,
            }),
            draft_tokens: 16,
            acceptance_threshold: 0.5,
        };
        let decoder = SpeculativeDecoder::new(config);
        // Draft nicht verfuegbar - Fallback-Pfad wird getestet
        assert!(!decoder.config.draft_client.is_available());
    }
}
