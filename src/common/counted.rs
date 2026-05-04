//! Byte-counting helpers for the data plane.
//!
//! [`TunnelCounters`] holds two atomics — bytes received from the QUIC peer
//! (`bytes_in`) and bytes sent to the QUIC peer (`bytes_out`) — that the
//! admin API exposes per [`crate::server::state::TunnelEntry`]. The
//! per-tunnel handlers ([`crate::common::tcp`], [`crate::common::udp`],
//! [`crate::common::socks`]) bump them on the hot path.
//!
//! For TCP-style copies [`CountedReader`] wraps an [`AsyncRead`] and bumps
//! a counter by the number of bytes filled into the supplied [`ReadBuf`] on
//! each successful poll. Datagram paths increment counters directly with
//! the datagram length.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

/// Per-tunnel byte counters. The atomics are wrapped in [`Arc`]s so the
/// data-plane wrappers ([`CountedReader`]) can share the *same* atomic
/// with the [`crate::server::state::TunnelEntry`] held by the admin API —
/// each handle is just an `Arc<AtomicU64>` clone.
///
/// Both atomics use [`Ordering::Relaxed`]: the admin API is an
/// observability surface, not a sync primitive, so a slightly stale read
/// is fine and we don't want to add memory-fence cost to the data plane.
#[derive(Debug, Default)]
pub struct TunnelCounters {
    bytes_in: Arc<AtomicU64>,
    bytes_out: Arc<AtomicU64>,
}

impl TunnelCounters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Cloneable handle for incrementing `bytes_in` (data received from
    /// the QUIC peer).
    pub fn in_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.bytes_in)
    }

    /// Cloneable handle for incrementing `bytes_out` (data sent to the
    /// QUIC peer).
    pub fn out_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.bytes_out)
    }

    pub fn add_in(&self, n: u64) {
        if n > 0 {
            self.bytes_in.fetch_add(n, Ordering::Relaxed);
        }
    }

    pub fn add_out(&self, n: u64) {
        if n > 0 {
            self.bytes_out.fetch_add(n, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.bytes_in.load(Ordering::Relaxed),
            self.bytes_out.load(Ordering::Relaxed),
        )
    }
}

/// AsyncRead wrapper that increments `counter` by the number of bytes the
/// inner reader filled into the supplied [`ReadBuf`] on each successful
/// poll. Requires `R: Unpin` so we can avoid pulling in `pin-project`.
pub struct CountedReader<R> {
    inner: R,
    counter: Arc<AtomicU64>,
}

impl<R> CountedReader<R> {
    pub fn new(inner: R, counter: Arc<AtomicU64>) -> Self {
        Self { inner, counter }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CountedReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let res = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(&res, Poll::Ready(Ok(()))) {
            let delta = buf.filled().len().saturating_sub(before);
            if delta > 0 {
                self.counter.fetch_add(delta as u64, Ordering::Relaxed);
            }
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn counted_reader_tallies_reads() {
        let data = b"hello world".to_vec();
        let counter = Arc::new(AtomicU64::new(0));
        let mut r = CountedReader::new(&data[..], counter.clone());
        let mut out = Vec::new();
        r.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, data);
        assert_eq!(counter.load(Ordering::Relaxed), data.len() as u64);
    }

    #[test]
    fn add_in_out_atomically() {
        let c = TunnelCounters::default();
        c.add_in(10);
        c.add_in(0);
        c.add_out(5);
        assert_eq!(c.snapshot(), (10, 5));
    }

    #[test]
    fn handles_share_underlying_atomic() {
        let c = TunnelCounters::default();
        let h = c.in_handle();
        h.fetch_add(7, Ordering::Relaxed);
        assert_eq!(c.snapshot(), (7, 0));
    }
}
