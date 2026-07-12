//! The embedded-server handshake.
//!
//! An SDK that bundles its server spawns it as a subprocess and has to
//! learn where it bound. Guessing a port races; polling one that was
//! already taken is worse. So the server binds where it likes — port 0 is
//! the point — and announces the result on stdout:
//!
//! ```text
//! soothfast-ready {"base_url":"http://127.0.0.1:54321"}
//! ```
//!
//! Launchers scan stdout for that prefix and ignore every other line, so a
//! server is free to log normally. The line is plain text on a pipe: the
//! embedded server does not have to use this crate, or be written in Rust
//! at all. [`announce`] is a convenience, not a requirement.

use std::io::Write;

/// The prefix identifying a readiness line. Everything after it is a JSON
/// object with a `base_url` field.
pub const READY_PREFIX: &str = "soothfast-ready ";

/// Tell a waiting SDK launcher which base URL this server is serving.
///
/// Call once, after the listener is bound and accepting. Writes to stdout
/// and flushes, since the launcher is blocked on this line.
///
/// ```no_run
/// let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
/// let port = listener.local_addr()?.port();
/// soothfast::embed::announce(&format!("http://127.0.0.1:{port}"));
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn announce(base_url: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(
        stdout,
        "{READY_PREFIX}{{\"base_url\":\"{}\"}}",
        escape(base_url)
    );
    let _ = stdout.flush();
}

/// Escape a string into a JSON string body. URLs rarely need it; a URL
/// built from user input might.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_urls_pass_through_untouched() {
        assert_eq!(escape("http://127.0.0.1:8080"), "http://127.0.0.1:8080");
        assert_eq!(escape("http://[::1]:80/base"), "http://[::1]:80/base");
    }

    #[test]
    fn a_url_can_never_break_out_of_the_json_string() {
        assert_eq!(escape("a\"b"), "a\\\"b");
        assert_eq!(escape("a\\b"), "a\\\\b");
        assert_eq!(escape("a\nb"), "a\\nb");
        assert_eq!(escape("a\u{1}b"), "a\\u0001b");
    }
}
