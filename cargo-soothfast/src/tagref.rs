//! Release tags of a GitHub repository, resolved to the commits they name.

use std::process::Command;

/// The commit the tag `tag` names in the GitHub repository `slug`, as
/// `git ls-remote` reports it. `None` when the remote cannot be reached or
/// has no such tag.
pub fn tag_sha(slug: &str, tag: &str) -> Option<String> {
    Command::new("git")
        .args(["ls-remote", &format!("https://github.com/{slug}")])
        .arg(format!("refs/tags/{tag}"))
        .arg(format!("refs/tags/{tag}^{{}}"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| tag_commit(&String::from_utf8_lossy(&o.stdout)))
}

/// The commit in `ls-remote` output: an annotated tag's peeled `^{}` line,
/// else the lightweight tag's own.
fn tag_commit(listing: &str) -> Option<String> {
    let refs: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(sha, _)| sha.len() == 40)
        .collect();
    refs.iter()
        .find(|(_, r)| r.ends_with("^{}"))
        .or_else(|| refs.first())
        .map(|(sha, _)| sha.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_annotated_tag_pins_the_commit_it_peels_to() {
        let tag = "a".repeat(40);
        let commit = "b".repeat(40);
        let listing = format!("{tag}\trefs/tags/v1\n{commit}\trefs/tags/v1^{{}}\n");
        assert_eq!(tag_commit(&listing), Some(commit.clone()));
        assert_eq!(
            tag_commit(&format!("{commit}\trefs/tags/v1\n")),
            Some(commit)
        );
        assert_eq!(tag_commit(""), None);
    }
}
