pub const GAIA_SYSTEM_PROMPT: &str = include_str!("../prompts/gaia-system.md");
pub const SETUP_INTERVIEW_PROMPT: &str = include_str!("../prompts/setup-interview.md");

pub fn required_prompts() -> [(&'static str, &'static str); 2] {
    [
        ("gaia-system", GAIA_SYSTEM_PROMPT),
        ("setup-interview", SETUP_INTERVIEW_PROMPT),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_present_and_guarded() {
        for (name, prompt) in required_prompts() {
            assert!(!prompt.trim().is_empty(), "{name} prompt must not be empty");
        }
        assert!(GAIA_SYSTEM_PROMPT.contains("sentinel-ctl"));
        assert!(GAIA_SYSTEM_PROMPT.contains("not an MCP server"));
        assert!(SETUP_INTERVIEW_PROMPT.contains("GaiaSpec"));
        assert!(SETUP_INTERVIEW_PROMPT.contains("company-context.md"));
    }
}
