//! Descriptor-relative filesystem access shared by native and component tools.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{mkdirat, openat2, statat, AtFlags, Dir, Mode, OFlags, ResolveFlags, CWD};

pub(crate) struct ScopedPath {
    root: File,
    relative: PathBuf,
    exact_leaf: bool,
}

fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn resolution() -> ResolveFlags {
    ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS
}

impl ScopedPath {
    pub(crate) fn new(root: &Path, relative: &Path) -> io::Result<Self> {
        if root.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(denied("filesystem path must stay within its declared root"));
        }
        let root = File::from(openat2(
            CWD,
            root,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )?);
        Ok(Self {
            root,
            exact_leaf: false,
            relative: if relative.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                relative.to_path_buf()
            },
        })
    }

    pub(crate) fn exact_leaf(mut self) -> Self {
        self.exact_leaf = true;
        self
    }

    fn open(&self, relative: &Path, flags: OFlags) -> io::Result<File> {
        let nonblocking = if flags.contains(OFlags::PATH) {
            OFlags::empty()
        } else {
            OFlags::NONBLOCK
        };
        Ok(File::from(openat2(
            &self.root,
            relative,
            flags
                | OFlags::CLOEXEC
                | nonblocking
                | if self.exact_leaf {
                    OFlags::NOFOLLOW
                } else {
                    OFlags::empty()
                },
            if flags.contains(OFlags::CREATE) {
                Mode::RUSR | Mode::WUSR
            } else {
                Mode::empty()
            },
            resolution(),
        )?))
    }

    fn regular(file: &File) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(denied("tool file must be regular and have one link"));
        }
        Ok(())
    }

    pub(crate) fn read_bytes(&self) -> io::Result<Vec<u8>> {
        let mut file = self.open(&self.relative, OFlags::RDONLY)?;
        Self::regular(&file)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub(crate) fn read_text(&self) -> io::Result<String> {
        String::from_utf8(self.read_bytes()?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    fn create_parents(&self) -> io::Result<()> {
        let mut parent = self.root.try_clone()?;
        let mut prefix = PathBuf::new();
        for component in self
            .relative
            .parent()
            .unwrap_or(Path::new("."))
            .components()
        {
            let Component::Normal(name) = component else {
                continue;
            };
            prefix.push(name);
            match self.open(&prefix, OFlags::PATH | OFlags::DIRECTORY) {
                Ok(directory) => parent = directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match mkdirat(&parent, name, Mode::RWXU) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                        Err(error) => return Err(error.into()),
                    }
                    parent = self.open(&prefix, OFlags::PATH | OFlags::DIRECTORY)?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(crate) fn write(&self, bytes: &[u8], create_parents: bool) -> io::Result<()> {
        if create_parents {
            self.create_parents()?;
        }
        // Truncate only the validated opened inode, never the unchecked path.
        let mut file = self.open(&self.relative, OFlags::WRONLY | OFlags::CREATE)?;
        Self::regular(&file)?;
        file.set_len(0)?;
        file.write_all(bytes)
    }

    pub(crate) fn list(&self) -> io::Result<Vec<String>> {
        let file = self.open(&self.relative, OFlags::RDONLY | OFlags::DIRECTORY)?;
        let mut directory = Dir::new(file)?;
        let mut names = Vec::new();
        for entry in &mut directory {
            let entry = entry?;
            if let Ok(name) = entry.file_name().to_str() {
                if name != "." && name != ".." {
                    names.push(name.to_owned());
                }
            }
        }
        Ok(names)
    }

    pub(crate) fn exists_or_new_leaf(&self) -> bool {
        match self.open(&self.relative, OFlags::PATH) {
            Ok(file) => !self.exact_leaf || Self::regular(&file).is_ok(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(leaf) = self.relative.file_name() else {
                    return false;
                };
                let parent = self
                    .relative
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let Ok(parent) = self.open(parent, OFlags::PATH | OFlags::DIRECTORY) else {
                    return false;
                };
                matches!(
                    statat(&parent, leaf, AtFlags::SYMLINK_NOFOLLOW),
                    Err(rustix::io::Errno::NOENT)
                )
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn descriptor_resolution_rejects_leaf_replacement_before_read_and_write() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), "inside").unwrap();
        fs::write(outside.path().join("file"), "outside").unwrap();
        let path = ScopedPath::new(root.path(), Path::new("file")).unwrap();
        assert!(path.exists_or_new_leaf());
        fs::remove_file(root.path().join("file")).unwrap();
        symlink(outside.path().join("file"), root.path().join("file")).unwrap();
        assert!(path.read_bytes().is_err());
        assert!(path.write(b"corrupted", false).is_err());
        assert_eq!(
            fs::read_to_string(outside.path().join("file")).unwrap(),
            "outside"
        );
    }

    #[test]
    fn descriptor_resolution_rejects_parent_replacement_and_dangling_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("parent")).unwrap();
        let path = ScopedPath::new(root.path(), Path::new("parent/new")).unwrap();
        fs::rename(root.path().join("parent"), root.path().join("old-parent")).unwrap();
        symlink(outside.path(), root.path().join("parent")).unwrap();
        assert!(path.write(b"escape", true).is_err());
        assert!(!outside.path().join("new").exists());
        symlink(outside.path().join("missing"), root.path().join("dangling")).unwrap();
        let dangling = ScopedPath::new(root.path(), Path::new("dangling")).unwrap();
        assert!(!dangling.exists_or_new_leaf());
        assert!(dangling.write(b"escape", false).is_err());
        assert!(!outside.path().join("missing").exists());
    }

    #[test]
    fn pinned_root_survives_root_path_replacement() {
        let container = tempfile::tempdir().unwrap();
        let root = container.path().join("root");
        let outside = container.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(root.join("file"), "inside").unwrap();
        fs::write(outside.join("file"), "outside").unwrap();
        let pinned = ScopedPath::new(&root, Path::new("file")).unwrap();
        fs::rename(&root, container.path().join("original")).unwrap();
        symlink(&outside, &root).unwrap();
        assert_eq!(pinned.read_text().unwrap(), "inside");
        assert!(ScopedPath::new(&root, Path::new("file")).is_err());
    }

    #[test]
    fn safe_internal_relative_symlinks_and_nested_creation_remain_supported() {
        let root = tempfile::tempdir().unwrap();
        let file = ScopedPath::new(root.path(), Path::new("nested/dir/file")).unwrap();
        file.write(b"inside", true).unwrap();
        symlink("nested/dir/file", root.path().join("alias")).unwrap();
        let alias = ScopedPath::new(root.path(), Path::new("alias")).unwrap();
        assert_eq!(alias.read_text().unwrap(), "inside");
        alias.write(b"updated", false).unwrap();
        assert_eq!(file.read_text().unwrap(), "updated");
        assert!(ScopedPath::new(root.path(), Path::new("../outside")).is_err());
    }

    #[test]
    fn hardlinks_and_non_regular_files_are_rejected_before_truncation() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("original"), "retained").unwrap();
        fs::hard_link(root.path().join("original"), root.path().join("alias")).unwrap();
        let alias = ScopedPath::new(root.path(), Path::new("alias")).unwrap();
        assert!(alias.write(b"corrupted", false).is_err());
        assert!(alias.read_bytes().is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("original")).unwrap(),
            "retained"
        );
        assert!(ScopedPath::new(root.path(), Path::new("."))
            .unwrap()
            .read_bytes()
            .is_err());
    }
}
