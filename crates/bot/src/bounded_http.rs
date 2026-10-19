//! Small HTTP/1 server with hard connection and header-reading bounds.

use std::{future::Future, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use axum::Router;
use hyper::server::conn::http1;
use hyper_util::{rt::TokioIo, rt::TokioTimer, service::TowerToHyperService};
use tokio::{
    sync::{watch, Semaphore},
    task::JoinSet,
};

const MAX_CONNECTIONS: usize = 64;
const MAX_HEADERS: usize = 32;
const MAX_HEADER_BYTES: usize = 8 * 1024;
#[cfg(not(test))]
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const HEADER_READ_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(100);

pub(crate) async fn serve<F>(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: F,
) -> Result<()>
where
    F: Future<Output = ()> + Send,
{
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result {
                    tracing::warn!(%error, "HTTP connection task failed");
                }
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted.context("accepting HTTP connection")?;
                let Ok(permit) = connections.clone().try_acquire_owned() else {
                    tracing::debug!(%peer, "rejected HTTP connection at capacity");
                    continue;
                };
                stream.set_nodelay(true).context("configuring HTTP connection")?;

                let app = app.clone();
                let mut shutdown_rx = shutdown_rx.clone();
                tasks.spawn(async move {
                    let mut builder = http1::Builder::new();
                    builder
                        .timer(TokioTimer::new())
                        .header_read_timeout(HEADER_READ_TIMEOUT)
                        .max_headers(MAX_HEADERS)
                        .max_buf_size(MAX_HEADER_BYTES);
                    let connection = builder.serve_connection(
                        TokioIo::new(stream),
                        TowerToHyperService::new(app),
                    );
                    tokio::pin!(connection);

                    tokio::select! {
                        result = connection.as_mut() => {
                            if let Err(error) = result {
                                tracing::debug!(%peer, %error, "closed HTTP connection");
                            }
                        }
                        changed = shutdown_rx.changed() => {
                            if changed.is_ok() {
                                connection.as_mut().graceful_shutdown();
                                if let Err(error) = connection.await {
                                    tracing::debug!(%peer, %error, "closed HTTP connection during shutdown");
                                }
                            }
                        }
                    }
                    drop(permit);
                });
            }
        }
    }

    let _ = shutdown_tx.send(true);
    if tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                tracing::warn!(%error, "HTTP connection task failed during shutdown");
            }
        }
    })
    .await
    .is_err()
    {
        tasks.abort_all();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::{http::StatusCode, routing::post};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        sync::oneshot,
        task::JoinHandle,
    };

    use super::*;

    async fn test_server() -> (SocketAddr, oneshot::Sender<()>, JoinHandle<Result<()>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let app = Router::new().route("/", post(|| async { StatusCode::NO_CONTENT }));
        let server = tokio::spawn(serve(listener, app, async move {
            let _ = shutdown_rx.await;
        }));
        (address, shutdown_tx, server)
    }

    async fn stop_server(shutdown: oneshot::Sender<()>, server: JoinHandle<Result<()>>) {
        let _ = shutdown.send(());
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("server shutdown must be bounded")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn closes_connections_with_incomplete_headers() {
        let (address, shutdown, server) = test_server().await;
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"POST / HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .unwrap();

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut response))
            .await
            .expect("header timeout must close the connection")
            .unwrap();
        assert!(!response.windows(6).any(|window| window == b"200 OK"));

        stop_server(shutdown, server).await;
    }

    #[tokio::test]
    async fn rejects_too_many_headers() {
        let (address, shutdown, server) = test_server().await;
        let mut stream = TcpStream::connect(address).await.unwrap();
        let mut request = String::from("POST / HTTP/1.1\r\nHost: localhost\r\n");
        for index in 0..MAX_HEADERS {
            request.push_str(&format!("X-Test-{index}: value\r\n"));
        }
        request.push_str("Content-Length: 0\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&response).contains("431 Request Header Fields Too Large"));

        stop_server(shutdown, server).await;
    }

    #[tokio::test]
    async fn rejects_oversized_headers() {
        let (address, shutdown, server) = test_server().await;
        let mut stream = TcpStream::connect(address).await.unwrap();
        let request = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nX-Fill: {}\r\nContent-Length: 0\r\n\r\n",
            "x".repeat(MAX_HEADER_BYTES)
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&response).contains("431 Request Header Fields Too Large"));

        stop_server(shutdown, server).await;
    }

    #[tokio::test]
    async fn rejects_connections_above_the_global_cap() {
        let (address, shutdown, server) = test_server().await;
        let mut connections = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            connections.push(TcpStream::connect(address).await.unwrap());
            tokio::task::yield_now().await;
        }

        let mut overflow = TcpStream::connect(address).await.unwrap();
        let mut byte = [0_u8; 1];
        assert!(
            tokio::time::timeout(Duration::from_millis(100), overflow.read(&mut byte))
                .await
                .is_ok(),
            "the connection above the cap must be closed immediately"
        );

        drop(connections);
        stop_server(shutdown, server).await;
    }

    #[tokio::test]
    async fn aborts_stalled_requests_during_shutdown() {
        let (address, shutdown, server) = test_server().await;
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\n")
            .await
            .unwrap();

        stop_server(shutdown, server).await;
    }
}
