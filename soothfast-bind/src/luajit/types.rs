//! The `require` path's own derived name: the embedded C crate's identity,
//! and the file path `require` resolves the `.lua` module against.

use crate::cabi::types::snake;
use crate::naming;

/// Lua's reserved words, plus the names this backend's own generated code
/// always binds at file scope (`ffi`, `lib`), around every method (`self`),
/// or inside a call's own body (`err`, `ret`): a parameter given one of
/// these names would shadow a local the same function still needs to reach.
/// `error` is reserved for the same reason: it is Lua's own raise builtin,
/// which every fallible call's body still calls by that bare name.
const RESERVED: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while", "self",
    "ffi", "lib", "err", "ret", "error",
];

/// A Rust parameter or function name as the Lua identifier it is declared
/// under.
pub(crate) fn lua_ident(name: &str) -> String {
    naming::escape(&snake(name), RESERVED)
}

/// The dotted `require` path's last segment, snake_cased and escaped: the
/// one derivation everything C-facing is named after (the embedded crate,
/// its header, its library), mirroring `cgo::types::package_name`.
pub(crate) fn module_name(package: &str) -> String {
    let last = package.rsplit('.').next().unwrap_or(package);
    naming::escape(&snake(last), RESERVED)
}

/// The dotted `require` path as the file path `require` resolves against:
/// its loader replaces every `.` with the directory separator before
/// searching `package.path`, so the glue must land there to match.
pub(crate) fn require_path(package: &str) -> String {
    format!("{}.lua", package.replace('.', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_module_name_is_the_require_paths_last_segment() {
        assert_eq!(module_name("acme.core"), "core");
        assert_eq!(module_name("acme-core"), "acme_core");
    }

    #[test]
    fn a_module_name_that_collides_with_a_reserved_word_is_escaped() {
        assert_eq!(module_name("acme.end"), "end_");
        assert_eq!(module_name("acme.ffi"), "ffi_");
    }

    #[test]
    fn the_require_path_nests_by_dot() {
        assert_eq!(require_path("acme.core"), "acme/core.lua");
        assert_eq!(require_path("acme"), "acme.lua");
    }

    #[test]
    fn reserved_words_and_generated_locals_gain_a_trailing_underscore() {
        assert_eq!(lua_ident("end"), "end_");
        assert_eq!(lua_ident("ffi"), "ffi_");
        assert_eq!(lua_ident("by"), "by");
    }
}
