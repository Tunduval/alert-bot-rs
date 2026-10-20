//! Small HTTP/1 server with hard connection, lifetime and header-reading bounds.

use std::{future::Future, sync::Arc, time::Duration};

use anyhow::Result;
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
// A client that pipelines requests and never reads the responses blocks the
// server in write, and one that reads a few bytes at a time keeps a write
// timeout from firing. Only a deadline on the whole connection frees the slot
// in both cases. Telegram reconnects without trouble, so a busy keep-alive
// connection is closed gracefully once the lifetime is up.
#[cfg(not(test))]
const CONNECTION_LIFETIME: Duration = Duration::from_secs(60);
#[cfg(test)]
const CONNECTION_LIFETIME: Duration = Duration::from_millis(500);
#[cfg(not(test))]
const CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_secs(1);

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
                let (stream, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        // An accept error must not stop the server. Errors that
                        // are not tied to one connection, such as running out
                        // of file descriptors, are retried after a pause.
                        tracing::warn!(%error, "accepting HTTP connection failed");
                        if !is_connection_error(&error) {
                            tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                        }
                        continue;
                    }
                };
                let Ok(permit) = connections.clone().try_acquire_owned() else {
                    tracing::debug!(%peer, "rejected HTTP connection at capacity");
                    continue;
                };
                if let Err(error) = stream.set_nodelay(true) {
                    tracing::debug!(%peer, %error, "configuring HTTP connection failed");
                    continue;
                }

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
                    let lifetime = tokio::time::sleep(CONNECTION_LIFETIME);
                    tokio::pin!(lifetime);

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
                        _ = &mut lifetime => {
                            // Let a request in flight finish, then drop the
                            // connection even if a client stalls the write.
                            connection.as_mut().graceful_shutdown();
                            match tokio::time::timeout(CONNECTION_DRAIN_TIMEOUT, connection).await {
                                Ok(Err(error)) => {
                                    tracing::debug!(%peer, %error, "closed HTTP connection at its lifetime");
                                }
                                Ok(Ok(())) => {}
                                Err(_) => {
                                    tracing::debug!(%peer, "dropped HTTP connection at its lifetime");
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

fn is_connection_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
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
        test_server_with(Router::new().route("/", post(|| async { StatusCode::NO_CONTENT }))).await
    }

    async fn test_server_with(
        app: Router,
    ) -> (SocketAddr, oneshot::Sender<()>, JoinHandle<Result<()>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
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

    /// Whether a fresh connection gets a response, i.e. a slot is free.
    async fn probe(address: SocketAddr) -> bool {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        let mut head = [0_u8; 12];
        matches!(
            tokio::time::timeout(Duration::from_millis(100), stream.read_exact(&mut head)).await,
            Ok(Ok(_))
        ) && &head == b"HTTP/1.1 200"
    }

    #[tokio::test]
    async fn frees_slots_held_by_clients_that_never_read_responses() {
        // Each response is far larger than the socket buffers, so a client
        // that pipelines requests and never reads blocks the server in write.
        let app = Router::new().route("/", post(|| async { vec![b'x'; 64 * 1024] }));
        let (address, shutdown, server) = test_server_with(app).await;
        let request = b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n";
        let mut slow_readers = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            let socket = tokio::net::TcpSocket::new_v4().unwrap();
            socket.set_recv_buffer_size(4096).unwrap();
            let mut stream = socket.connect(address).await.unwrap();
            stream.write_all(&request.repeat(100)).await.unwrap();
            slow_readers.push(stream);
        }
        assert!(
            !probe(address).await,
            "slow readers must occupy every connection slot"
        );

        let freed = tokio::time::timeout(Duration::from_secs(3), async {
            while !probe(address).await {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            freed.is_ok(),
            "the connection lifetime must free slots held by slow readers"
        );

        drop(slow_readers);
        stop_server(shutdown, server).await;
    }

    #[test]
    fn only_per_connection_accept_errors_skip_the_backoff() {
        use std::io::{Error, ErrorKind};

        for kind in [
            ErrorKind::ConnectionRefused,
            ErrorKind::ConnectionAborted,
            ErrorKind::ConnectionReset,
        ] {
            assert!(is_connection_error(&Error::from(kind)), "{kind:?}");
        }
        // EMFILE: the process has run out of file descriptors.
        assert!(!is_connection_error(&Error::from_raw_os_error(24)));
    }
}
