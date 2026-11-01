//! Throwaway directories and git repositories for tests.

use std::path::{Path, PathBuf};

use crate::invoke;

/// A temporary directory, emptied on creation and removed when dropped,
/// whether the test passed or failed.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> TempDir {
        let root = std::env::temp_dir().join(format!("soothfast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        TempDir(root)
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct TempRepo {
    pub root: PathBuf,
    _dir: TempDir,
}

impl TempRepo {
    pub fn new(name: &str) -> TempRepo {
        let dir = TempDir::new(name);
        std::fs::create_dir_all(&*dir).unwrap();
        let repo = TempRepo {
            root: dir.to_path_buf(),
            _dir: dir,
        };
        repo.git(&["init", "-q"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["config", "tag.gpgsign", "false"]);
        repo
    }

    pub fn git(&self, args: &[&str]) -> String {
        invoke::git_in(&self.root, args).unwrap()
    }

    pub fn write(&self, path: &str, content: &str) {
        let path = self.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    pub fn commit(&self) {
        self.git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "x",
        ]);
    }
}
