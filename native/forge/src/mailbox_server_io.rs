//! Bound accepted A2A connections and close clients that stall during headers.

use axum::serve::Listener;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{sleep, Instant, Sleep};

const MAX_CONNECTIONS: usize = 16;
const IDLE_TIMEOUT: Duration = Duration::from_secs(12);

pub struct BoundedListener {
    inner: TcpListener,
    permits: Arc<Semaphore>,
}

impl BoundedListener {
    pub fn new(inner: TcpListener) -> Self {
        Self {
            inner,
            permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }
    }
}

pub struct BoundedStream {
    inner: TcpStream,
    idle: Pin<Box<Sleep>>,
    _permit: OwnedSemaphorePermit,
}

impl Listener for BoundedListener {
    type Io = BoundedStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let permit = self
                .permits
                .clone()
                .acquire_owned()
                .await
                .expect("listener connection semaphore remains open");
            match self.inner.accept().await {
                Ok((inner, addr)) => {
                    let stream = BoundedStream {
                        inner,
                        idle: Box::pin(sleep(IDLE_TIMEOUT)),
                        _permit: permit,
                    };
                    return (stream, addr);
                }
                Err(error) => {
                    eprintln!("A2A accept error: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

impl AsyncRead for BoundedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.idle.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "A2A client read idle timeout",
            )));
        }
        let filled = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > filled {
            self.idle.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
        }
        result
    }
}

impl AsyncWrite for BoundedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn stalled_headers_timeout_and_release_connection_capacity() {
        runtime().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _client = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let mut bounded = BoundedListener::new(listener);
            let (mut stream, _) = bounded.accept().await;
            assert_eq!(bounded.permits.available_permits(), MAX_CONNECTIONS - 1);
            stream.idle = Box::pin(sleep(Duration::from_millis(10)));
            let mut bytes = [0; 1];
            let mut buffer = ReadBuf::new(&mut bytes);
            let error = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, &mut buffer))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            drop(stream);
            assert_eq!(bounded.permits.available_permits(), MAX_CONNECTIONS);
        });
    }

    #[test]
    fn accepted_connections_remain_bounded_until_a_client_closes() {
        runtime().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let mut bounded = BoundedListener::new(listener);
            let mut clients = Vec::new();
            let mut streams = Vec::new();
            for _ in 0..MAX_CONNECTIONS {
                clients.push(TcpStream::connect(address).await.unwrap());
                streams.push(bounded.accept().await.0);
            }
            clients.push(TcpStream::connect(address).await.unwrap());
            assert!(
                tokio::time::timeout(Duration::from_millis(25), bounded.accept())
                    .await
                    .is_err()
            );
            streams.pop();
            let next = tokio::time::timeout(Duration::from_secs(1), bounded.accept())
                .await
                .unwrap();
            assert_eq!(bounded.permits.available_permits(), 0);
            drop(next);
            drop(streams);
            assert_eq!(bounded.permits.available_permits(), MAX_CONNECTIONS);
        });
    }
}
