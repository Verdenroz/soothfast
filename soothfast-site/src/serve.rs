//! `cargo soothfast docs serve` — the dev server: serve the built site over
//! HTTP, poll sources for changes, rebuild, and push a live-reload event to
//! open pages. Hand-rolled on `std::net` (no hyper/tokio, per the
//! dependency policy); binds loopback by default — this is a dev tool, the
//! published site is static files.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::build::BuildReport;

/// Client injected into every served HTML page: reload when the build
/// generation changes. Absolute path so page depth doesn't matter.
const LIVERELOAD: &str = "<script>(function(){var g=null;\
var es=new EventSource(\"/__soothfast/events\");\
es.onmessage=function(e){if(g===null){g=e.data;return;}\
if(e.data!==g)location.reload();};})();</script>";

/// How often sources are polled and SSE clients are checked.
const POLL: Duration = Duration::from_millis(300);

/// A bound dev server, not yet running (split from `run` so callers and
/// tests can learn the actual address before the loop starts).
pub struct Server {
    listener: TcpListener,
}

impl Server {
    /// Bind `addr` ("127.0.0.1:8000"; port 0 picks a free one).
    pub fn bind(addr: &str) -> Result<Server, String> {
        let listener = TcpListener::bind(addr).map_err(|e| format!("cannot bind {addr}: {e}"))?;
        Ok(Server { listener })
    }

    /// The address actually bound.
    pub fn addr(&self) -> String {
        self.listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default()
    }

    /// Serve `site_dir` forever: accept connections on a thread, watch
    /// `watch` paths here, calling `rebuild` on any change. A failed
    /// rebuild keeps serving the previous site and logs the error.
    pub fn run(
        self,
        site_dir: &Path,
        watch: &[PathBuf],
        mut rebuild: impl FnMut() -> Result<BuildReport, String>,
    ) -> Result<(), String> {
        let generation = Arc::new(AtomicU64::new(1));
        let gen_accept = generation.clone();
        let dir = site_dir.to_path_buf();
        std::thread::spawn(move || {
            for stream in self.listener.incoming().flatten() {
                let dir = dir.clone();
                let generation = gen_accept.clone();
                std::thread::spawn(move || handle(stream, &dir, &generation));
            }
        });

        let mut state = snapshot(watch);
        loop {
            std::thread::sleep(POLL);
            let current = snapshot(watch);
            if current == state {
                continue;
            }
            state = current;
            eprintln!("soothfast: change detected — rebuilding");
            match rebuild() {
                Ok(report) => {
                    generation.fetch_add(1, Ordering::SeqCst);
                    eprintln!(
                        "soothfast: rebuilt {} page(s), reloading browsers",
                        report.pages
                    );
                }
                Err(e) => eprintln!("soothfast: rebuild failed (serving last good build): {e}"),
            }
        }
    }
}

/// (path → (mtime, len)) for every file under the watched roots.
fn snapshot(watch: &[PathBuf]) -> BTreeMap<PathBuf, (SystemTime, u64)> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<PathBuf> = watch.to_vec();
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if let Ok(meta) = std::fs::metadata(&path) {
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.insert(path, (mtime, meta.len()));
        }
    }
    out
}

fn handle(stream: TcpStream, site_dir: &Path, generation: &AtomicU64) {
    let Ok(peer) = stream.peer_addr() else { return };
    let _ = peer;
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Drain headers; the dev server doesn't need any of them.
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok() && line != "\r\n" && !line.is_empty() {
        line.clear();
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    if method != "GET" && method != "HEAD" {
        respond(
            &stream,
            405,
            "text/plain",
            b"method not allowed",
            method == "HEAD",
        );
        return;
    }
    let path = target.split('?').next().unwrap_or("/");

    if path == "/__soothfast/events" {
        sse(stream, generation);
        return;
    }

    match resolve(site_dir, path) {
        Some(file) => {
            let Ok(mut body) = std::fs::read(&file) else {
                respond(
                    &stream,
                    404,
                    "text/html",
                    NOT_FOUND.as_bytes(),
                    method == "HEAD",
                );
                return;
            };
            let mime = mime_for(&file);
            if mime == "text/html" {
                body = inject_livereload(body);
            }
            respond(&stream, 200, mime, &body, method == "HEAD");
        }
        None => respond(
            &stream,
            404,
            "text/html",
            NOT_FOUND.as_bytes(),
            method == "HEAD",
        ),
    }
}

/// Map a request path to a file under the site dir. Rejects traversal;
/// directories serve their index.html.
fn resolve(site_dir: &Path, path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(path);
    if decoded.contains('\0') || decoded.split('/').any(|seg| seg == "..") {
        return None;
    }
    let rel = decoded.trim_start_matches('/');
    let mut file = site_dir.join(rel);
    if rel.is_empty() || decoded.ends_with('/') || file.is_dir() {
        file = file.join("index.html");
    }
    if !file.is_file() {
        // Pretty-URL fallback: /measuring → /measuring/index.html.
        let with_index = file.join("index.html");
        if with_index.is_file() {
            return Some(with_index);
        }
        return None;
    }
    Some(file)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Insert the live-reload client before `</body>` (or append when absent).
fn inject_livereload(mut body: Vec<u8>) -> Vec<u8> {
    let needle = b"</body>";
    if let Some(at) = body.windows(needle.len()).rposition(|w| w == needle) {
        body.splice(at..at, LIVERELOAD.bytes());
    } else {
        body.extend_from_slice(LIVERELOAD.as_bytes());
    }
    body
}

fn respond(mut stream: &TcpStream, status: u16, mime: &str, body: &[u8], head_only: bool) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if !head_only {
        let _ = stream.write_all(body);
    }
}

/// Server-sent events: emit the current generation immediately, then again
/// whenever it changes; the client reloads on any difference.
fn sse(mut stream: TcpStream, generation: &AtomicU64) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                Cache-Control: no-store\r\nConnection: keep-alive\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut last = generation.load(Ordering::SeqCst);
    if stream
        .write_all(format!("data: {last}\n\n").as_bytes())
        .is_err()
    {
        return;
    }
    let mut ticks = 0u32;
    loop {
        std::thread::sleep(POLL);
        let current = generation.load(Ordering::SeqCst);
        if current != last {
            last = current;
            if stream
                .write_all(format!("data: {current}\n\n").as_bytes())
                .is_err()
            {
                return;
            }
        }
        ticks += 1;
        // Periodic comment keeps intermediaries from timing the stream out
        // and detects closed connections.
        if ticks.is_multiple_of(50) && stream.write_all(b": ping\n\n").is_err() {
            return;
        }
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

const NOT_FOUND: &str = "<!DOCTYPE html><meta charset=\"utf-8\">\
<title>404</title><body style=\"font-family:monospace;padding:4rem\">\
<h1>404</h1><p>No such page in the built site.</p></body>";

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn resolve_rejects_traversal_and_serves_indexes() {
        let dir = std::env::temp_dir().join(format!("soothfast-serve-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("guide"));
        std::fs::write(dir.join("index.html"), "root").unwrap();
        std::fs::write(dir.join("guide/index.html"), "guide").unwrap();
        std::fs::write(dir.join("a b.txt"), "space").unwrap();

        assert!(resolve(&dir, "/").unwrap().ends_with("index.html"));
        assert!(
            resolve(&dir, "/guide/")
                .unwrap()
                .ends_with("guide/index.html")
        );
        assert!(
            resolve(&dir, "/guide")
                .unwrap()
                .ends_with("guide/index.html")
        );
        assert!(resolve(&dir, "/a%20b.txt").is_some());
        assert!(resolve(&dir, "/../secret").is_none());
        assert!(resolve(&dir, "/%2e%2e/secret").is_none());
        assert!(resolve(&dir, "/nope").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn livereload_is_injected_before_body_close() {
        let html = b"<html><body><p>x</p></body></html>".to_vec();
        let out = String::from_utf8(inject_livereload(html)).unwrap();
        assert!(out.contains("EventSource"));
        assert!(out.ends_with("</body></html>"));
        // No </body>: script appended.
        let out = String::from_utf8(inject_livereload(b"plain".to_vec())).unwrap();
        assert!(out.starts_with("plain<script>"));
    }

    #[test]
    fn server_smoke_get_and_reload_event() {
        let dir = std::env::temp_dir().join(format!("soothfast-srv2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("index.html"), "<body>hi</body>").unwrap();

        let server = Server::bind("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let dir2 = dir.clone();
        std::thread::spawn(move || {
            let _ = server.run(&dir2, &[], || {
                Ok(BuildReport {
                    warnings: Vec::new(),
                    pages: 0,
                    assets: 0,
                })
            });
        });

        let mut conn = TcpStream::connect(&addr).unwrap();
        conn.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut buf = String::new();
        conn.read_to_string(&mut buf).unwrap();
        assert!(buf.starts_with("HTTP/1.1 200"), "{buf}");
        assert!(buf.contains("EventSource")); // livereload injected
        assert!(buf.contains("hi"));

        // SSE endpoint sends the current generation immediately.
        let mut conn = TcpStream::connect(&addr).unwrap();
        conn.write_all(b"GET /__soothfast/events HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut reader = BufReader::new(conn);
        let mut lines = Vec::new();
        for _ in 0..12 {
            let mut l = String::new();
            if reader.read_line(&mut l).is_err() || l.is_empty() {
                break;
            }
            let done = l.starts_with("data:");
            lines.push(l);
            if done {
                break;
            }
        }
        assert!(lines.iter().any(|l| l.starts_with("data: 1")), "{lines:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
