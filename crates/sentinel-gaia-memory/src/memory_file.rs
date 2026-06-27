//! Markdown memory file support for Gaia Console Memory.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::MEMORY_FILE_NAME;

const TEMPLATE: &str = "# Gaia Console Memory\n\
\n\
## Setup Decisions\n\
\n\
## Open Tasks\n\
\n\
## User Preferences\n\
\n\
## Notes\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemorySection {
    SetupDecisions,
    OpenTasks,
    UserPreferences,
    Notes,
}

impl MemorySection {
    pub fn heading(self) -> &'static str {
        match self {
            Self::SetupDecisions => "Setup Decisions",
            Self::OpenTasks => "Open Tasks",
            Self::UserPreferences => "User Preferences",
            Self::Notes => "Notes",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::SetupDecisions => "setup-decisions",
            Self::OpenTasks => "open-tasks",
            Self::UserPreferences => "user-preferences",
            Self::Notes => "notes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub section: MemorySection,
    pub timestamp_ms: u64,
    pub text: String,
}

pub struct GaiaConsoleMemoryFile {
    path: PathBuf,
}

impl GaiaConsoleMemoryFile {
    pub fn open_or_create(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir).with_context(|| {
            format!(
                "create Gaia Console Memory data directory {}",
                data_dir.display()
            )
        })?;
        let path = data_dir.join(MEMORY_FILE_NAME);
        if !path.exists() {
            fs::write(&path, TEMPLATE)
                .with_context(|| format!("create Gaia Console Memory file {}", path.display()))?;
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read_full(&self) -> anyhow::Result<String> {
        fs::read_to_string(&self.path)
            .with_context(|| format!("read Gaia Console Memory file {}", self.path.display()))
    }

    pub fn read_condensed(&self, max_bytes: usize) -> anyhow::Result<String> {
        let contents = self.read_full()?;
        if contents.len() <= max_bytes {
            return Ok(contents);
        }
        let mut end = max_bytes;
        while end > 0 && !contents.is_char_boundary(end) {
            end -= 1;
        }
        Ok(contents[..end].to_string())
    }

    pub fn append_entry(
        &self,
        section: MemorySection,
        timestamp_ms: u64,
        text: impl AsRef<str>,
    ) -> anyhow::Result<MemoryEntry> {
        let text = text.as_ref().trim();
        if text.is_empty() {
            bail!("Gaia Console Memory entry text must not be empty");
        }

        let entry = MemoryEntry {
            section,
            timestamp_ms,
            text: text.to_string(),
        };
        let contents = self.read_full()?;
        let line = format!("- {}: {}\n", timestamp_ms, normalize_line(text));
        let updated = append_to_section(contents, section.heading(), &line);
        fs::write(&self.path, updated).with_context(|| {
            format!(
                "append Gaia Console Memory entry to {}",
                self.path.display()
            )
        })?;
        Ok(entry)
    }
}

fn append_to_section(mut contents: String, heading: &str, line: &str) -> String {
    let marker = format!("## {heading}\n");
    let Some(marker_start) = contents.find(&marker) else {
        if !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push('\n');
        contents.push_str(&marker);
        contents.push('\n');
        contents.push_str(line);
        return contents;
    };

    let body_start = marker_start + marker.len();
    let next_heading = contents[body_start..]
        .find("\n## ")
        .map(|offset| body_start + offset + 1)
        .unwrap_or(contents.len());

    let mut updated = String::with_capacity(contents.len() + line.len() + 1);
    updated.push_str(&contents[..next_heading]);
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(line);
    updated.push_str(&contents[next_heading..]);
    updated
}

fn normalize_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_file_creates_required_sections() {
        let dir = tempfile::tempdir().unwrap();
        let file = GaiaConsoleMemoryFile::open_or_create(dir.path()).unwrap();
        let contents = file.read_full().unwrap();

        assert!(file.path().ends_with(MEMORY_FILE_NAME));
        for section in [
            MemorySection::SetupDecisions,
            MemorySection::OpenTasks,
            MemorySection::UserPreferences,
            MemorySection::Notes,
        ] {
            assert!(
                contents.contains(&format!("## {}", section.heading())),
                "missing {}",
                section.heading()
            );
        }
    }

    #[test]
    fn memory_file_appends_entry_under_requested_section() {
        let dir = tempfile::tempdir().unwrap();
        let file = GaiaConsoleMemoryFile::open_or_create(dir.path()).unwrap();

        let entry = file
            .append_entry(
                MemorySection::OpenTasks,
                1_234,
                "  verify backup scheduling in #442  ",
            )
            .unwrap();

        assert_eq!(entry.section, MemorySection::OpenTasks);
        let contents = file.read_full().unwrap();
        let open_tasks = contents.find("## Open Tasks").unwrap();
        let user_preferences = contents.find("## User Preferences").unwrap();
        let section = &contents[open_tasks..user_preferences];
        assert!(section.contains("- 1234: verify backup scheduling in #442"));
    }

    #[test]
    fn memory_file_condensed_read_is_bounded_at_utf8_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let file = GaiaConsoleMemoryFile::open_or_create(dir.path()).unwrap();
        file.append_entry(MemorySection::Notes, 2_000, "alpha beta gamma")
            .unwrap();

        let condensed = file.read_condensed(17).unwrap();
        assert!(condensed.len() <= 17);
        assert!(std::str::from_utf8(condensed.as_bytes()).is_ok());
    }
}
