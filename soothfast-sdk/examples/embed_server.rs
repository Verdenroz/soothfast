//! A stand-in for the server an embedding SDK bundles.
//!
//! Serves the routes the golden fixture describes, over a hand-rolled
//! HTTP/1.1 responder so the fixture costs no dependencies. The part under
//! test is the first line it prints: `soothfast::embed::announce` is the
//! handshake every generated launcher waits on.
//!
//! Build with `cargo build -p soothfast-sdk --example embed_server`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();

    // Prove the launcher tolerates ordinary logging before the handshake.
    println!("embed_server: starting up");
    soothfast::embed::announce(&format!("http://127.0.0.1:{port}"));

    for stream in listener.incoming() {
        let stream = stream?;
        // Serial is fine: the launcher tests are one request at a time.
        if let Err(e) = serve(stream) {
            eprintln!("embed_server: {e}");
        }
    }
    Ok(())
}

fn serve(mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        {
            content_length = value.1.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    // A server that logs every request is the normal case, and the pipe it
    // logs into holds 64 KiB. Both streams, so a launcher that drains only
    // one still wedges here.
    if std::env::var_os("EMBED_SERVER_CHATTY").is_some() {
        let filler = "x".repeat(1024);
        println!("embed_server: {method} {target} {filler}");
        eprintln!("embed_server: {method} {target} {filler}");
    }

    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let (status, payload) = route(&method, path, query, &body);
    respond(&mut stream, status, payload.as_deref())
}

/// The golden fixture's five operations, plus enough of a pager to walk.
fn route(method: &str, path: &str, query: &str, body: &[u8]) -> (u16, Option<String>) {
    match (method, path) {
        ("DELETE", _) => (204, None),
        ("POST", "/v1/items") => {
            let name = String::from_utf8_lossy(body).to_string();
            (201, Some(format!("{{\"id\":7,\"note\":{name:?}}}")))
        }
        // Echoes the environment it was launched with, so a test can prove
        // a client's `server_env` reached the process it spawned.
        ("GET", "/v1/stats") => {
            let note = std::env::var("EMBED_SERVER_NOTE").unwrap_or_default();
            (
                200,
                Some(format!(
                    "{{\"status\":\"ok\",\"data\":{{\"id\":1,\"note\":{note:?}}}}}"
                )),
            )
        }
        ("GET", "/v1/items") if query.contains("cursor=c1") => (
            200,
            Some(
                "{\"items\":[{\"id\":2}],\"pageInfo\":{\"endCursor\":null,\"hasNextPage\":false}}"
                    .into(),
            ),
        ),
        ("GET", "/v1/items") if query.contains("limit=") => (
            200,
            Some(
                "{\"items\":[{\"id\":1}],\"pageInfo\":{\"endCursor\":\"c1\",\"hasNextPage\":true}}"
                    .into(),
            ),
        ),
        ("GET", "/v1/items") => (200, Some("[{\"id\":1},{\"id\":2}]".into())),
        ("GET", p) if p.starts_with("/v1/items/") => {
            let id = p.trim_start_matches("/v1/items/");
            if id == "nope" {
                return (
                    404,
                    Some("{\"error\":\"not_found\",\"message\":\"no such item\"}".into()),
                );
            }
            (
                200,
                Some(format!(
                    "{{\"id\":1,\"from\":\"x\",\"logoUrl\":\"a\",\"logo_url\":\"b\",\
                     \"note\":{id:?}}}"
                )),
            )
        }
        _ => (404, Some("{\"error\":\"not_found\"}".into())),
    }
}

fn respond(stream: &mut TcpStream, status: u16, payload: Option<&str>) -> std::io::Result<()> {
    let body = payload.unwrap_or("");
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\n\
         content-type: application/json\r\n\
         content-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        reason(status),
        body.len(),
    )?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        _ => "Not Found",
    }
}
