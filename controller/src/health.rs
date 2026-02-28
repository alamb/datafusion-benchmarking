//! Minimal TCP-based health server for Kubernetes probes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::info;

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const NOT_FOUND: &[u8] = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
const UNAVAILABLE: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 9\r\n\r\nnot ready";

/// Shared flag set to `true` once both loops have started.
pub type ReadyFlag = Arc<AtomicBool>;

pub fn ready_flag() -> ReadyFlag {
    Arc::new(AtomicBool::new(false))
}

/// Listen on `0.0.0.0:8080` and serve `/healthz` (liveness) and `/readyz` (readiness).
pub async fn serve(token: CancellationToken, ready: ReadyFlag) {
    let listener = TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("failed to bind health port 8080");
    info!("health server listening on :8080");

    loop {
        let (mut stream, _addr) = tokio::select! {
            res = listener.accept() => match res {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::warn!(error = %e, "health accept error");
                    continue;
                }
            },
            _ = token.cancelled() => break,
        };

        let ready = ready.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 256];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
            let req = String::from_utf8_lossy(&buf);

            let response = if req.starts_with("GET /healthz") {
                OK
            } else if req.starts_with("GET /readyz") {
                if ready.load(Ordering::Relaxed) {
                    OK
                } else {
                    UNAVAILABLE
                }
            } else {
                NOT_FOUND
            };

            let _ = stream.write_all(response).await;
            let _ = stream.shutdown().await;
        });
    }

    info!("health server stopped");
}
