//! A throwaway git repository for selection and key tests.

use std::path::PathBuf;

use crate::invoke;

pub struct TempRepo {
    pub root: PathBuf,
}

impl TempRepo {
    pub fn new(name: &str) -> TempRepo {
        let root = std::env::temp_dir().join(format!("soothfast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let repo = TempRepo { root };
        repo.git(&["init", "-q"]);
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

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
