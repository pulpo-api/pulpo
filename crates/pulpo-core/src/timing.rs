//! Measures where the time of a request goes: DNS (a resolver that times lookups) and the
//! connection, TCP plus TLS (a layer around reqwest's connector). The rest is the server
//! and the download, measured around the request itself.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

/// Time spent so far, in microseconds; a request with redirects adds up its connections.
#[derive(Debug, Default)]
pub(crate) struct Probe {
    dns_us: AtomicU64,
    connect_us: AtomicU64,
}

impl Probe {
    pub(crate) fn dns_ms(&self) -> u64 {
        self.dns_us.load(Ordering::Relaxed) / 1000
    }

    /// Connection time without DNS (the connector resolves names itself).
    pub(crate) fn connect_ms(&self) -> u64 {
        self.connect_us
            .load(Ordering::Relaxed)
            .saturating_sub(self.dns_us.load(Ordering::Relaxed))
            / 1000
    }
}

pub(crate) struct TimedResolver(pub(crate) Arc<Probe>);

impl reqwest::dns::Resolve for TimedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let probe = self.0.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let started = Instant::now();
            let addrs: Vec<_> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            probe
                .dns_us
                .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Wraps the connector: every call opens one connection (TCP, then TLS for https).
#[derive(Clone)]
pub(crate) struct TimingLayer(pub(crate) Arc<Probe>);

impl<S> tower_layer::Layer<S> for TimingLayer {
    type Service = TimingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TimingService {
            inner,
            probe: self.0.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct TimingService<S> {
    inner: S,
    probe: Arc<Probe>,
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

impl<S, R> tower_service::Service<R> for TimingService<S>
where
    S: tower_service::Service<R>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Result<S::Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: R) -> Self::Future {
        let started = Instant::now();
        let future = self.inner.call(request);
        let probe = self.probe.clone();
        Box::pin(async move {
            let result = future.await;
            probe
                .connect_us
                .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
            result
        })
    }
}
