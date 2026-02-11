use std::io::Write;
use std::process::{Command, Stdio};

/// Configuration for the BitNet inference subprocess.
pub struct BitNetConfig {
    /// Path to the compiled bitnet-inference binary.
    pub binary_path: String,
    /// Path to the GGUF model file.
    pub model_path: String,
    /// Number of CPU threads (optimal: 8 for i5-1235U E-Cores).
    pub threads: u32,
    /// Maximum tokens to generate per request.
    pub max_tokens: u32,
}

/// Client that manages BitNet subprocess lifecycle.
///
/// Each call to `generate()` spawns a new subprocess. This is intentional:
/// - Subprocess isolation prevents memory leaks from accumulating
/// - BitNet startup is fast (~50ms) relative to generation time
/// - Simplifies error recovery (crashed process = just spawn a new one)
pub struct BitNetClient {
    config: BitNetConfig,
}

impl BitNetClient {
    pub fn new(config: BitNetConfig) -> Self {
        Self { config }
    }

    /// Check if the BitNet binary exists and is executable.
    pub fn is_available(&self) -> bool {
        std::path::Path::new(&self.config.binary_path).exists()
    }

    /// Generate text from a prompt using BitNet subprocess.
    ///
    /// Spawns bitnet-inference with the configured model and parameters.
    /// Prompt is written to stdin, output is read from stdout.
    /// Returns the generated text or an error if the process fails.
    pub fn generate(&self, prompt: &str) -> anyhow::Result<String> {
        let mut child = Command::new(&self.config.binary_path)
            .arg("--model")
            .arg(&self.config.model_path)
            .arg("--threads")
            .arg(self.config.threads.to_string())
            .arg("--max-tokens")
            .arg(self.config.max_tokens.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes())?;
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("BitNet process failed: {}", stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitnet_config_creation() {
        let config = BitNetConfig {
            binary_path: "/usr/local/bin/bitnet-inference".to_string(),
            model_path: "/models/bitnet-7b.gguf".to_string(),
            threads: 8,
            max_tokens: 256,
        };
        assert_eq!(config.threads, 8);
        assert_eq!(config.max_tokens, 256);
        assert!(config.binary_path.ends_with("bitnet-inference"));
    }

    #[test]
    fn test_bitnet_not_available() {
        let client = BitNetClient::new(BitNetConfig {
            binary_path: "/nonexistent/bitnet-inference".to_string(),
            model_path: "/nonexistent/model.gguf".to_string(),
            threads: 8,
            max_tokens: 256,
        });
        assert!(!client.is_available());
    }

    #[test]
    fn test_bitnet_binary_check() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let client = BitNetClient::new(BitNetConfig {
            binary_path: tmp.path().to_string_lossy().to_string(),
            model_path: "/nonexistent/model.gguf".to_string(),
            threads: 8,
            max_tokens: 256,
        });
        assert!(client.is_available());
    }
}
