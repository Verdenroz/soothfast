//! Workspaces and scratch repos the init tests share.

use std::path::PathBuf;

use super::ci::Ci;
use super::ecosystem::{Ecosystem, Project, Settings, Tests};
use super::workflow::Workspace;

pub(super) fn ws(benched: &[&str], docs: Option<&str>) -> Workspace {
    Workspace {
        rust: true,
        projects: Vec::new(),
        ci: Ci::default(),
        default_branch: "master".into(),
        benched: benched.iter().map(|s| s.to_string()).collect(),
        docs_packages: docs.map(String::from).into_iter().collect(),
        action: "Verdenroz/soothfast@v0".into(),
        bot: false,
        labels: false,
        java: "21".into(),
        node: "22".into(),
    }
}

pub(super) fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soothfast-init-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn project(eco: Ecosystem, dir: &str) -> Project {
    let settings = Settings {
        bench_cmd: (eco == Ecosystem::Gradle).then(|| "./gradlew jmh -Pout={out}".into()),
        ..Settings::default()
    };
    Project {
        ecosystem: eco,
        dir: dir.into(),
        bench: settings.bench(eco).ok(),
        walltime: None,
        prefix: None,
        test: Tests::default(),
    }
}

pub(super) fn python_ws() -> Workspace {
    Workspace {
        rust: false,
        projects: vec![project(Ecosystem::Python, ".")],
        ..ws(&[], None)
    }
}

pub(super) fn mixed_ws(rust: bool, projects: Vec<Project>) -> Workspace {
    Workspace {
        rust,
        projects,
        ..ws(&["a"], None)
    }
}
