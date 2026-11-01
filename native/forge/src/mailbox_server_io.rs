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
    idle_timeout: Duration,
    header_timeout: Option<Duration>,
}

impl BoundedListener {
    pub fn new(inner: TcpListener) -> Self {
        Self {
            inner,
            permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            idle_timeout: IDLE_TIMEOUT,
            header_timeout: None,
        }
    }

    #[allow(dead_code)] // Also compiled by the mailbox binary; broker uses this constructor.
    pub fn with_limits(inner: TcpListener, max_connections: usize, idle_timeout: Duration) -> Self {
        Self {
            inner,
            permits: Arc::new(Semaphore::new(max_connections)),
            idle_timeout,
            header_timeout: Some(Duration::from_secs(5)),
        }
    }
}

pub struct BoundedStream {
    inner: TcpStream,
    idle: Pin<Box<Sleep>>,
    idle_timeout: Duration,
    header: Option<Header>,
    _permit: OwnedSemaphorePermit,
}

struct Header {
    deadline: Pin<Box<Sleep>>,
    seen: usize,
    tail: u32,
}

impl Header {
    fn observe(&mut self, bytes: &[u8]) -> io::Result<bool> {
        for byte in bytes {
            self.seen += 1;
            if self.seen > 16384 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP headers exceed 16 KiB",
                ));
            }
            self.tail = (self.tail << 8) | u32::from(*byte);
            if self.tail == 0x0d0a0d0a {
                return Ok(true);
            }
        }
        Ok(false)
    }
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
                        idle: Box::pin(sleep(self.idle_timeout)),
                        idle_timeout: self.idle_timeout,
                        header: self.header_timeout.map(|timeout| Header {
                            deadline: Box::pin(sleep(timeout)),
                            seen: 0,
                            tail: 0,
                        }),
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
        if self
            .header
            .as_mut()
            .is_some_and(|h| h.deadline.as_mut().poll(cx).is_ready())
        {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP header deadline expired",
            )));
        }
        if self.idle.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "A2A client read idle timeout",
            )));
        }
        let filled = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > filled {
            if let Some(header) = &mut self.header {
                match header.observe(&buf.filled()[filled..]) {
                    Ok(true) => self.header = None,
                    Ok(false) => (),
                    Err(error) => return Poll::Ready(Err(error)),
                }
            }
            let timeout = self.idle_timeout;
            self.idle.as_mut().reset(Instant::now() + timeout);
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
        if self.idle.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP write deadline expired",
            )));
        }
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
    fn broker_header_deadline_is_absolute_and_header_size_is_bounded() {
        runtime().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut client = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let mut bounded = BoundedListener::with_limits(listener, 1, Duration::from_secs(300));
            let (mut stream, _) = bounded.accept().await;
            stream.header.as_mut().unwrap().deadline = Box::pin(sleep(Duration::from_millis(15)));
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            client.write_all(b"GET /health HTTP/1.1\r\n").await.unwrap();
            let mut bytes = [0; 128];
            assert!(stream.read(&mut bytes).await.unwrap() > 0);
            tokio::time::sleep(Duration::from_millis(20)).await;
            client.write_all(b"X: still-slow\r\n").await.unwrap();
            assert_eq!(
                stream.read(&mut bytes).await.unwrap_err().kind(),
                io::ErrorKind::TimedOut
            );
            drop(stream);
            assert_eq!(bounded.permits.available_permits(), 1);
            let mut header = Header {
                deadline: Box::pin(sleep(Duration::from_secs(5))),
                seen: 0,
                tail: 0,
            };
            assert!(!header.observe(b"GET / HTTP/1.1\r\n\r").unwrap());
            assert!(header.observe(b"\nbody").unwrap());
            let mut header = Header {
                deadline: Box::pin(sleep(Duration::from_secs(5))),
                seen: 0,
                tail: 0,
            };
            assert!(header.observe(&vec![b'x'; 16385]).is_err());
        });
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
