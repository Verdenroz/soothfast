use super::{Ctx, Row, State};
use crate::invoke::lock_packages;

const SUBJECT: &str = "cli version";

/// This CLI against the `soothfast` version Cargo.lock pins.
pub(super) fn check(ctx: &Ctx) -> Vec<Row> {
    let lock = std::fs::read_to_string(ctx.root.join("Cargo.lock")).unwrap_or_default();
    vec![against(&lock, env!("CARGO_PKG_VERSION"))]
}

fn against(lock: &str, cli: &str) -> Row {
    let Some(pin) = lock_packages(lock)
        .into_iter()
        .find(|p| p.name == "soothfast")
    else {
        return Row::ok(SUBJECT, format!("{cli}, Cargo.lock pins no soothfast"));
    };
    if pin.version == cli {
        return Row::ok(SUBJECT, format!("{cli}, matches Cargo.lock"));
    }
    Row::needs(
        State::Stale,
        SUBJECT,
        format!("cli {cli}, Cargo.lock pins {}", pin.version),
        format!(
            "cargo install --locked cargo-soothfast --version {}",
            pin.version
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = "[[package]]\nname = \"soothfast\"\nversion = \"0.5.5\"\n";

    #[test]
    fn a_matching_pin_is_ok() {
        assert_eq!(against(LOCK, "0.5.5").state, State::Ok);
    }

    #[test]
    fn a_different_pin_is_stale_with_the_install_command() {
        let row = against(LOCK, "0.5.6");
        assert_eq!(row.state, State::Stale);
        assert_eq!(
            row.fix.as_deref(),
            Some("cargo install --locked cargo-soothfast --version 0.5.5")
        );
    }

    #[test]
    fn a_lock_without_soothfast_is_ok() {
        assert_eq!(against("", "0.5.6").state, State::Ok);
    }

    #[test]
    fn the_check_reads_cargo_lock_under_the_root() {
        let root = crate::status::scratch("version");
        std::fs::write(root.join("Cargo.lock"), LOCK).unwrap();
        let rows = check(&Ctx {
            root,
            packages: Vec::new(),
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, State::Stale);
    }
}
