//! Ruby bindings, over magnus and rb_sys.
//!
//! A Ruby `Array` boxes every element, and a `String`'s bytes may move
//! under a compacting collector, so unlike Python or Node there is no
//! pointer this backend can honestly hand over: every buffer is copied both
//! ways, the same answer wasm gives for its own reason. `bind gen` gives
//! Ruby no buffer advice for it, same as wasm.

mod glue;
mod package;
mod types;

use crate::naming;
use crate::plan::BindingPlan;
use crate::{BindFileSet, BindOptions};

/// The magnus release the generated glue builds against, unless the
/// `[[bind]]` entry pins another.
pub(crate) const DEFAULT_VERSION: &str = "0.8";

/// The rb-sys release the generated glue builds against. Not pinned by
/// `backend_version`: that knob follows magnus, the crate the surface
/// actually touches, and rb-sys is only its build-time companion.
pub(crate) const RB_SYS_VERSION: &str = "0.9";

const KEYWORDS: &[&str] = &[
    "BEGIN",
    "END",
    "__ENCODING__",
    "__FILE__",
    "__LINE__",
    "alias",
    "and",
    "begin",
    "break",
    "case",
    "class",
    "def",
    "defined?",
    "do",
    "else",
    "elsif",
    "end",
    "ensure",
    "false",
    "for",
    "if",
    "in",
    "module",
    "next",
    "nil",
    "not",
    "or",
    "redo",
    "rescue",
    "retry",
    "return",
    "self",
    "super",
    "then",
    "true",
    "undef",
    "unless",
    "until",
    "when",
    "while",
    "yield",
];

/// A Rust name as the Ruby identifier it is exported under.
pub(crate) fn rb_ident(name: &str) -> String {
    naming::escape(name, KEYWORDS)
}

/// Emit a complete Ruby binding package: a gem around a magnus glue crate.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let mut out = BindFileSet {
        notes: notes(plan),
        ..BindFileSet::default()
    };
    let module = types::module_name(&opts.module);
    let files = &mut out.files;
    files.insert(format!("{}.gemspec", opts.package), package::gemspec(opts));
    files.insert("Gemfile".into(), package::gemfile());
    files.insert("Rakefile".into(), package::rakefile(&opts.module));
    files.insert("README.md".into(), package::readme(plan, opts, &module));
    files.insert(".gitignore".into(), "target/\ntmp/\n*.gem\n".into());
    files.insert(
        format!("ext/{}/Cargo.toml", opts.module),
        package::cargo_toml(opts),
    );
    files.insert(
        format!("ext/{}/extconf.rb", opts.module),
        package::extconf(&opts.module),
    );
    files.insert(
        format!("ext/{}/src/lib.rs", opts.module),
        glue::render(plan, opts, &module),
    );
    files.insert(
        format!("lib/{}.rb", opts.module),
        package::lib_entry(&opts.module),
    );
    Ok(out)
}

/// Shapes Ruby cannot take as precisely as the Rust states them.
///
/// No transfer notes: `BufferSupport::AlwaysCopies` already makes
/// [`crate::plan::transfer_notes`] return none, and it is the honest answer
/// here too, not just an economical one — a Ruby `Array` has to be unboxed
/// element by element whether a parameter borrows or not, and a `String`'s
/// bytes are not a pointer this backend can keep past the call.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out: Vec<String> = plan
        .classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from Ruby",
                c.name
            )
        })
        .collect();
    out.extend(crate::plan::transfer_notes(plan));
    out
}
