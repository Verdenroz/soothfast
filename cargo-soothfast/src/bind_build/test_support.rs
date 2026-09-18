use std::path::Path;

/// `bind gen` always leaves a lockfile beside a cargo-driven glue
/// crate's manifest; a bare golden copy has none, and `--locked` needs
/// one to build against.
pub(super) fn lock(glue: &Path) {
    let status = crate::invoke::cargo_command()
        .arg("generate-lockfile")
        .current_dir(glue)
        .status()
        .expect("runs cargo generate-lockfile");
    assert!(status.success(), "cargo generate-lockfile failed");
}

pub(super) fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("makes dir");
    for entry in std::fs::read_dir(from)
        .expect("reads dir")
        .filter_map(Result::ok)
    {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copies file");
        }
    }
}
