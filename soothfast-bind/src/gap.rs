//! Places the binding surface could not be derived, and why.
//!
//! Every gap names the exact item or type so the report is actionable, and
//! the item it names is left unbound rather than bound to a guess.

/// A place an exported item could not be lowered into a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gap {
    /// A type outside the package's rustdoc index with no table mapping.
    UnmappedForeign {
        /// Where it appeared, e.g. `crate::counter::Counter::bump`.
        at: String,
        /// Canonical path, e.g. `chrono::DateTime`.
        path: String,
    },
    /// A type the language's binding layer cannot carry across.
    UnsupportedByBackend {
        at: String,
        /// How the type reads in Rust.
        ty: String,
        /// The language that cannot take it.
        lang: &'static str,
        why: String,
    },
    /// `impl Trait` / `dyn Trait` in return position: only the bound survives.
    Erased {
        at: String,
        /// The trait bound rustdoc did preserve.
        bound: String,
    },
    /// A generic item. Nothing says which instantiations to bind.
    Generic {
        at: String,
        /// The type parameter that stayed open.
        param: String,
    },
    /// A method taking `self` by value. A binding holds the instance, so
    /// moving out of it would leave the handle pointing at nothing.
    ConsumingReceiver { at: String },
    /// An `async fn` taking `&mut self`. The instance stays exclusively
    /// borrowed across every await point, which fails at runtime the moment
    /// anything else touches the handle.
    AsyncExclusiveReceiver { at: String },
    /// An `async fn` whose future cannot cross threads, which every binding
    /// layer that drives futures requires.
    NotSendStatic {
        at: String,
        /// The offending parameter or return type.
        ty: String,
    },
    /// An exported type crossing by value, as a field read out or an
    /// argument taken by value. Either would copy it, which needs a bound the
    /// exported type does not have to carry.
    HandleByValue {
        at: String,
        /// The exported type being copied.
        ty: String,
    },
}

impl Gap {
    /// Where the gap was found, for grouping and reporting.
    pub fn at(&self) -> &str {
        match self {
            Self::UnmappedForeign { at, .. }
            | Self::UnsupportedByBackend { at, .. }
            | Self::Erased { at, .. }
            | Self::Generic { at, .. }
            | Self::NotSendStatic { at, .. }
            | Self::HandleByValue { at, .. }
            | Self::ConsumingReceiver { at }
            | Self::AsyncExclusiveReceiver { at } => at,
        }
    }

    /// One-line explanation naming the cause and the remedy.
    pub fn explain(&self) -> String {
        match self {
            Self::UnmappedForeign { at, path } => format!(
                "{at}: foreign type `{path}` has no mapping; add one under \
                 [bind.types] in soothfast.toml"
            ),
            Self::UnsupportedByBackend { at, ty, lang, why } => {
                format!("{at}: `{ty}` cannot cross into {lang}: {why}")
            }
            Self::Erased { at, bound } => format!(
                "{at}: returns an erased type (bound `{bound}`), whose concrete \
                 shape is not in rustdoc JSON; name it in the signature to bind it"
            ),
            Self::Generic { at, param } => format!(
                "{at}: type parameter `{param}` stayed open; bindings need a \
                 concrete signature, so export a monomorphic wrapper instead"
            ),
            Self::ConsumingReceiver { at } => format!(
                "{at}: takes `self` by value, which would empty the handle the \
                 binding holds; take `&self` and return a new value instead"
            ),
            Self::AsyncExclusiveReceiver { at } => format!(
                "{at}: `async fn` taking `&mut self` holds the borrow across \
                 every await, which fails at runtime; take `&self` instead"
            ),
            Self::NotSendStatic { at, ty } => format!(
                "{at}: `{ty}` makes the future neither `Send` nor `'static`, \
                 which the binding layer requires to drive it"
            ),
            Self::HandleByValue { at, ty } => format!(
                "{at}: crosses the exported type `{ty}` by value, which would \
                 copy it; take it by reference, or add a method returning what \
                 the caller needs"
            ),
        }
    }
}
