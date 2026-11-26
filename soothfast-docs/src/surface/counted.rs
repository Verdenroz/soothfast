//! The items coverage counts: public items and inherent methods, once each
//! however many `pub use` paths spell them.

use std::collections::{BTreeMap, HashMap};

use super::{ItemInfo, Surface};

/// One distinct public item or inherent method.
pub struct Counted<'a> {
    /// The shortest spelling, for display.
    pub path: String,
    /// Every spelling, for lookups: a `covers=` tag or a bind may name any.
    pub aliases: Vec<&'a String>,
    /// What the surface records about the item.
    pub info: &'a ItemInfo,
}

impl Surface {
    /// Every item and every pub inherent method, deduplicated across `pub
    /// use` aliases and in path order. Trait-impl methods are never indexed
    /// and rustdoc omits `#[doc(hidden)]` items, so neither is counted.
    /// Associated consts are left out: coverage counts callables and types.
    pub fn counted(&self) -> Vec<Counted<'_>> {
        let groups = self.grouped();
        let mut owner: HashMap<&str, usize> = HashMap::new();
        for (i, group) in groups.iter().enumerate() {
            owner.extend(group.aliases.iter().map(|a| (a.as_str(), i)));
        }
        let mut methods: BTreeMap<(usize, &str), Vec<&String>> = BTreeMap::new();
        for (path, info) in &self.members {
            if info.kind != "function" {
                continue;
            }
            let Some((ty, name)) = path.rsplit_once("::") else {
                continue;
            };
            if let Some(&i) = owner.get(ty) {
                methods.entry((i, name)).or_default().push(path);
            }
        }
        let items = groups.iter().map(|g| Counted {
            path: g.representative.clone(),
            aliases: g.aliases.clone(),
            info: &self.items[g.representative],
        });
        let methods = methods.into_iter().map(|((i, name), aliases)| Counted {
            path: format!("{}::{name}", groups[i].representative),
            info: &self.members[aliases[0]],
            aliases,
        });
        let mut out: Vec<_> = items.chain(methods).collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(kind: &str, fingerprint: u64) -> ItemInfo {
        ItemInfo {
            kind: kind.into(),
            fingerprint,
            signature: String::new(),
            has_docs: false,
        }
    }

    fn surface() -> Surface {
        let mut s = Surface::default();
        for path in ["a::Thing", "a::inner::Thing"] {
            s.items.insert(path.into(), info("struct", 1));
        }
        s.items.insert("a::free".into(), info("function", 2));
        for ty in ["a::Thing", "a::inner::Thing"] {
            s.members.insert(format!("{ty}::new"), info("function", 3));
            s.members.insert(format!("{ty}::MAX"), info("constant", 4));
        }
        s
    }

    fn paths(s: &Surface) -> Vec<String> {
        s.counted().into_iter().map(|c| c.path).collect()
    }

    #[test]
    fn an_inherent_method_counts_once_under_its_types_shortest_path() {
        let surface = surface();
        assert_eq!(paths(&surface), ["a::Thing", "a::Thing::new", "a::free"]);
        let counted = surface.counted();
        assert_eq!(counted[1].aliases.len(), 2);
        assert_eq!(counted[1].info.kind, "function");
    }

    #[test]
    fn same_text_methods_of_different_types_stay_apart() {
        let mut s = Surface::default();
        for (ty, fingerprint) in [("a::X", 10), ("a::Y", 11)] {
            s.items.insert(ty.into(), info("struct", fingerprint));
            s.members.insert(format!("{ty}::len"), info("function", 7));
        }
        assert_eq!(paths(&s), ["a::X", "a::X::len", "a::Y", "a::Y::len"]);
    }

    #[test]
    fn a_member_of_an_unknown_type_is_not_counted() {
        let mut s = Surface::default();
        s.members.insert("a::Gone::new".into(), info("function", 3));
        assert!(s.counted().is_empty());
    }
}
