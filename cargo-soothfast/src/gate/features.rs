//! The triage note for a gate whose base and head resolve different features.

use crate::config::{Config, Group};
use crate::invoke::{self, CommonArgs};

/// The leading triage line when `base` and `head` name different feature
/// sets, `None` when they are the same set in any order.
pub(super) fn shift_note(base: Option<&str>, head: Option<&str>) -> Option<String> {
    fn sorted(f: Option<&str>) -> Vec<&str> {
        let mut names: Vec<&str> = f
            .into_iter()
            .flat_map(|f| f.split([',', ' ']))
            .filter(|n| !n.is_empty())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
    if sorted(base) == sorted(head) {
        return None;
    }
    let shown = |f: Option<&str>| f.filter(|f| !f.is_empty()).unwrap_or("default").to_string();
    Some(format!(
        "feature set changed between base and head ({} -> {}): codegen of unchanged code can shift under LTO",
        shown(base),
        shown(head)
    ))
}

/// [`shift_note`] for a gate against `refname`: the merge-base's own
/// `soothfast.toml` resolves the base side, the flags in `common` the head's.
pub(super) fn shift_note_against(common: &CommonArgs, refname: &str) -> Option<String> {
    let base = invoke::git(&["merge-base", "HEAD", refname]).ok()?;
    let text = invoke::git(&["show", &format!("{base}:soothfast.toml")]).unwrap_or_default();
    let pkgs: Vec<String> = common.pkg.iter().cloned().collect();
    let settings = Config::from_text(text).resolve(&pkgs, Group::Gate).ok()?;
    shift_note(settings.features.as_deref(), common.features.as_deref())
}

#[cfg(test)]
mod tests {
    use super::shift_note;

    #[test]
    fn different_feature_sets_name_both_sides() {
        assert_eq!(
            shift_note(Some("a"), Some("a,big")).as_deref(),
            Some(
                "feature set changed between base and head (a -> a,big): codegen of unchanged code can shift under LTO"
            )
        );
        assert!(
            shift_note(None, Some("big"))
                .unwrap()
                .contains("(default -> big)")
        );
    }

    #[test]
    fn the_same_set_in_any_order_is_no_change() {
        assert_eq!(shift_note(Some("a,b"), Some("b a")), None);
        assert_eq!(shift_note(None, Some("")), None);
    }
}
