//! What a prebuild run wants built, and the steps that follow from what the
//! stores and run cache already hold.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Lane {
    Bench,
    Routes,
    Rustdoc,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Side {
    Head,
    Base,
}

/// One thing a later job reads from a store.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Item {
    Bench {
        pkg: String,
        side: Side,
    },
    Routes {
        pkg: String,
        target: String,
    },
    Rustdoc {
        pkg: String,
        private: bool,
        features: Option<String>,
        /// A workspace crate documented beside a spec's own package, whose
        /// failure costs that crate's types their schemas and nothing else.
        aux: bool,
    },
}

impl Item {
    pub(super) fn pkg(&self) -> &str {
        match self {
            Item::Bench { pkg, .. } | Item::Routes { pkg, .. } | Item::Rustdoc { pkg, .. } => pkg,
        }
    }
}

impl Item {
    /// The item without what does not tell two requests for it apart.
    pub(super) fn identity(&self) -> Item {
        match self {
            Item::Rustdoc {
                pkg,
                private,
                features,
                ..
            } => Item::Rustdoc {
                pkg: pkg.clone(),
                private: *private,
                features: features.clone(),
                aux: false,
            },
            other => other.clone(),
        }
    }
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::Bench { pkg, side } => {
                let side = if *side == Side::Head {
                    "head"
                } else {
                    "merge-base"
                };
                write!(f, "bench executable {pkg} ({side})")
            }
            Item::Routes { pkg, target } => write!(f, "routes executable {pkg} ({target})"),
            Item::Rustdoc { pkg, private, .. } => {
                let scope = if *private { "private" } else { "public" };
                write!(f, "rustdoc json {pkg} ({scope} items)")
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Action {
    Build,
    Stored,
    /// Nothing is needed: the gate would not build it.
    Covered,
}

#[derive(Debug, PartialEq)]
pub(super) struct Step {
    pub(super) item: Item,
    pub(super) action: Action,
}

#[derive(Clone, Debug)]
pub(super) struct SpecPackage {
    pub(super) pkg: String,
    pub(super) target: String,
}

/// What the caller asked to have ready.
#[derive(Clone)]
pub(super) struct Wanted {
    pub(super) lanes: BTreeSet<Lane>,
    pub(super) packages: Vec<String>,
    pub(super) docs: Vec<String>,
    pub(super) spec: Vec<SpecPackage>,
    pub(super) features: Option<String>,
    /// The `--features` the caller gave, which is all `docs check` uses: it
    /// never reads the `[gate]` table.
    pub(super) docs_features: Option<String>,
    pub(super) against_ref: Option<String>,
    pub(super) reuse_base: bool,
    pub(super) stage: Option<PathBuf>,
    /// Legs the gate is split into, which decide the run-cache keys a stored
    /// merge-base run is looked up under.
    pub(super) shards: u64,
}

/// What the stores and the run cache hold.
pub(super) trait State {
    fn stored(&self, item: &Item) -> bool;
    /// Whether the run cache holds the merge-base run the gate would reuse
    /// for `pkg`.
    fn reference_stored(&self, pkg: &str) -> bool;
    /// The workspace crates to document with `pkg`, or `None` when none of
    /// its specs is generated from the code.
    fn generated_from(&self, pkg: &str) -> Option<Vec<String>>;
}

pub(super) fn step(item: Item, state: &impl State) -> Step {
    let action = if state.stored(&item) {
        Action::Stored
    } else {
        Action::Build
    };
    Step { item, action }
}

/// The steps for HEAD's tree, in the order they run.
pub(super) fn select(wanted: &Wanted, state: &impl State) -> Vec<Step> {
    let mut items = Vec::new();
    if wanted.lanes.contains(&Lane::Bench) {
        items.extend(wanted.packages.iter().map(|pkg| Item::Bench {
            pkg: pkg.clone(),
            side: Side::Head,
        }));
    }
    if wanted.lanes.contains(&Lane::Routes) {
        items.extend(wanted.spec.iter().map(|s| Item::Routes {
            pkg: s.pkg.clone(),
            target: s.target.clone(),
        }));
    }
    if wanted.lanes.contains(&Lane::Rustdoc) {
        let rustdoc =
            |pkg: &str, private: bool, features: Option<String>, aux: bool| Item::Rustdoc {
                pkg: pkg.to_string(),
                private,
                features,
                aux,
            };
        items.extend(
            wanted
                .docs
                .iter()
                .map(|pkg| rustdoc(pkg, false, wanted.docs_features.clone(), false)),
        );
        for s in &wanted.spec {
            let Some(aux) = state.generated_from(&s.pkg) else {
                continue;
            };
            items.push(rustdoc(&s.pkg, true, wanted.features.clone(), false));
            items.extend(aux.iter().map(|pkg| rustdoc(pkg, true, None, true)));
        }
    }
    // One item per crate, visibility and features; it is an aux crate only if
    // every requester wanted it as one.
    let mut seen: BTreeMap<Item, usize> = BTreeMap::new();
    let mut unique: Vec<Item> = Vec::new();
    for item in items {
        match seen.get(&item.identity()) {
            Some(&at) => {
                if let (Item::Rustdoc { aux, .. }, Item::Rustdoc { aux: wanted, .. }) =
                    (&mut unique[at], &item)
                {
                    *aux &= *wanted;
                }
            }
            None => {
                seen.insert(item.identity(), unique.len());
                unique.push(item);
            }
        }
    }
    unique.into_iter().map(|item| step(item, state)).collect()
}

/// The steps for the merge-base's tree. A package whose merge-base run is
/// stored needs no executable of that side.
pub(super) fn select_base(wanted: &Wanted, state: &impl State) -> Vec<Step> {
    if !wanted.lanes.contains(&Lane::Bench) || wanted.against_ref.is_none() {
        return Vec::new();
    }
    wanted
        .packages
        .iter()
        .map(|pkg| {
            let item = Item::Bench {
                pkg: pkg.clone(),
                side: Side::Base,
            };
            if wanted.reuse_base && state.reference_stored(pkg) {
                return Step {
                    item,
                    action: Action::Covered,
                };
            }
            step(item, state)
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub(super) enum Outcome {
    Built,
    Stored,
    Covered,
    /// Built, but the store did not keep it; the consumer builds it again.
    Unstored,
    Failed(String),
}
