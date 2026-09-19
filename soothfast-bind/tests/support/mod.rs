//! Shared by every smoke test that can skip a missing host toolchain.

use std::env;

/// Whether the test should proceed. `false` means skip, allowed only when
/// `SOOTHFAST_SMOKE_STRICT` is unset. CI sets that variable so a missing
/// toolchain fails the test naming `tool` instead of skipping it, since a
/// silent skip there would mean the glue never actually got compiled.
pub fn require_toolchain(available: bool, tool: &str, install_hint: &str) -> bool {
    require_toolchain_with(available, tool, install_hint, strict_mode())
}

fn strict_mode() -> bool {
    env::var_os("SOOTHFAST_SMOKE_STRICT").is_some()
}

fn require_toolchain_with(available: bool, tool: &str, install_hint: &str, strict: bool) -> bool {
    if available {
        return true;
    }
    if strict {
        panic!("`{tool}` not found on PATH: {install_hint} (SOOTHFAST_SMOKE_STRICT is set)");
    }
    eprintln!("skipping: `{tool}` not found on PATH: {install_hint}");
    false
}

#[cfg(test)]
mod tests {
    use super::require_toolchain_with;

    #[test]
    fn skips_when_not_strict() {
        assert!(!require_toolchain_with(
            false,
            "toolx",
            "install toolx",
            false
        ));
    }

    #[test]
    #[should_panic(expected = "toolx")]
    fn fails_when_strict() {
        require_toolchain_with(false, "toolx", "install toolx", true);
    }
}
