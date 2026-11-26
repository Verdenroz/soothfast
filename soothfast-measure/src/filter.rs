//! `--filter` matching: substring alternatives with `^`/`$` anchors.

/// Whether `id` matches `filter`: any `|`-separated alternative that is a
/// substring of `id`, with a leading `^` pinning it to the start and a
/// trailing `$` to the end.
pub fn matches(filter: &str, id: &str) -> bool {
    filter.split('|').any(|alt| {
        let (alt, start) = alt.strip_prefix('^').map_or((alt, false), |a| (a, true));
        let (alt, end) = alt.strip_suffix('$').map_or((alt, false), |a| (a, true));
        match (start, end) {
            (true, true) => id == alt,
            (true, false) => id.starts_with(alt),
            (false, true) => id.ends_with(alt),
            (false, false) => id.contains(alt),
        }
    })
}

/// Whether `filter` holds a regex metacharacter this matcher reads literally.
pub fn has_unsupported_syntax(filter: &str) -> bool {
    filter.contains(['(', ')', '[', ']', '*', '+', '?', '.'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_filter_is_a_substring() {
        assert!(matches("fnv", "soothfast_registry::fnv1a_short"));
        assert!(!matches("fnv", "soothfast_registry::block_on"));
    }

    #[test]
    fn pipes_separate_alternatives() {
        assert!(matches("fnv|block_on", "a::block_on"));
        assert!(matches("fnv|block_on", "a::fnv1a"));
        assert!(!matches("fnv|block_on", "a::other"));
    }

    #[test]
    fn anchors_pin_an_alternative_to_an_end() {
        assert!(matches("^a::", "a::x"));
        assert!(!matches("^x", "a::x"));
        assert!(matches("x$", "a::x"));
        assert!(!matches("a$", "a::x"));
        assert!(matches("^a::x$", "a::x"));
        assert!(!matches("^a::$", "a::x"));
    }

    #[test]
    fn anchors_apply_per_alternative() {
        assert!(matches("^zzz|x$", "a::x"));
        assert!(!matches("^zzz|y$", "a::x"));
    }

    #[test]
    fn other_metacharacters_are_flagged_and_read_literally() {
        assert!(has_unsupported_syntax("ind_(a|b)"));
        assert!(has_unsupported_syntax("a.*b"));
        assert!(!has_unsupported_syntax("^a::b$|c"));
        assert!(!matches("ind_(a|b)", "m::ind_a"));
    }
}
