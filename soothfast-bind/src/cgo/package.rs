//! `go.mod` for the generated package.

use crate::{BindOptions, GENERATED_GO};

/// The lowest Go version the generated code needs. The `bufPtr` helper is
/// generic, which is the newest language feature in play.
const GO_VERSION: &str = "1.21";

pub(crate) fn go_mod(opts: &BindOptions) -> String {
    format!(
        "{GENERATED_GO}\nmodule {}\n\ngo {GO_VERSION}\n",
        opts.package
    )
}
