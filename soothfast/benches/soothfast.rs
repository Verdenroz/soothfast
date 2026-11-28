//! Async benches on a caller-supplied tokio runtime against a loopback
//! upstream that lives on its own thread.

soothfast::bench_main!();

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

struct Upstream(SocketAddr);

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Serves `RESPONSE` forever; each request also makes `extra_allocs` boxed
/// allocations so the item's cost is known to include server-side work.
fn serve(extra_allocs: usize) -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = [0u8; 256];
            let _ = stream.read(&mut buf).unwrap();
            for _ in 0..extra_allocs {
                std::hint::black_box(Box::new(0u8));
            }
            stream.write_all(RESPONSE).unwrap();
        }
    });
    Upstream(addr)
}

async fn light_upstream() -> Upstream {
    serve(0)
}

async fn heavy_upstream() -> Upstream {
    serve(1000)
}

async fn get(upstream: &Upstream) -> usize {
    let mut conn = TcpStream::connect(upstream.0).await.unwrap();
    conn.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
    let mut body = Vec::new();
    conn.read_to_end(&mut body).await.unwrap();
    body.len()
}

#[soothfast::bench(group = "executor", executor = runtime, setup = light_upstream)]
async fn get_light(upstream: &Upstream) -> usize {
    soothfast::keep(get(upstream).await)
}

#[soothfast::bench(group = "executor", executor = runtime, setup = heavy_upstream)]
async fn get_heavy(upstream: &Upstream) -> usize {
    soothfast::keep(get(upstream).await)
}
