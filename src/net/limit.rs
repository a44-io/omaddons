//! `Limited`: at most [`MAX_CONCURRENT_REQUESTS`] requests in flight overall and
//! [`MAX_CONCURRENT_PER_HOST`] to any one host (vision §5.13), so a parallel fetch never
//! trips github.com's undocumented secondary limits. A request holds its permits until
//! its response body is dropped; redirects run below this layer inside the same permits.
//! The host permit is taken before the global one, so requests waiting on a busy host
//! never hold global slots that other hosts could use.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::guard::parse_target;
use super::{Http, Request, Response};
use crate::error::Result;
use crate::politeness::{MAX_CONCURRENT_PER_HOST, MAX_CONCURRENT_REQUESTS};

/// A counting semaphore.
struct Semaphore {
    in_use: Mutex<usize>,
    freed: Condvar,
    max: usize,
}

impl Semaphore {
    fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            in_use: Mutex::new(0),
            freed: Condvar::new(),
            max: max.max(1),
        })
    }

    fn lock(&self) -> MutexGuard<'_, usize> {
        self.in_use
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn acquire(self: &Arc<Self>) -> Permit {
        let mut in_use = self.lock();
        while *in_use >= self.max {
            in_use = self
                .freed
                .wait(in_use)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *in_use += 1;
        Permit(Arc::clone(self))
    }
}

/// One slot of a semaphore, freed on drop.
struct Permit(Arc<Semaphore>);

impl Drop for Permit {
    fn drop(&mut self) {
        let mut in_use = self.0.lock();
        *in_use = in_use.saturating_sub(1);
        self.0.freed.notify_one();
    }
}

/// A response body that holds its request's permits.
struct Held {
    body: Box<dyn Read + Send>,
    _permits: (Permit, Permit),
}

impl Read for Held {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.body.read(buf)
    }
}

/// Caps requests in flight, globally and per host.
pub struct Limited {
    inner: Box<dyn Http>,
    global: Arc<Semaphore>,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    per_host: usize,
}

impl Limited {
    pub fn new(inner: Box<dyn Http>) -> Self {
        Self::with_limits(inner, MAX_CONCURRENT_REQUESTS, MAX_CONCURRENT_PER_HOST)
    }

    pub fn with_limits(inner: Box<dyn Http>, global: usize, per_host: usize) -> Self {
        Self {
            inner,
            global: Semaphore::new(global),
            hosts: Mutex::new(HashMap::new()),
            per_host,
        }
    }

    fn host(&self, url: &str) -> Arc<Semaphore> {
        let host = parse_target(url, 0).map(|t| t.host).unwrap_or_default();
        let mut hosts = self
            .hosts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(
            hosts
                .entry(host)
                .or_insert_with(|| Semaphore::new(self.per_host)),
        )
    }
}

impl Http for Limited {
    fn send(&self, req: Request) -> Result<Response> {
        let host = self.host(&req.url).acquire();
        let global = self.global.acquire();
        let mut resp = self.inner.send(req)?;
        let body = resp.take_body();
        resp.set_body(Box::new(Held {
            body,
            _permits: (host, global),
        }));
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    /// Holds each request for a moment and records the most in flight, overall and per
    /// host.
    #[derive(Default)]
    struct Counting {
        now: AtomicUsize,
        peak: AtomicUsize,
        per_host: Mutex<HashMap<String, (usize, usize)>>,
    }

    struct Slow(Arc<Counting>);

    impl Http for Slow {
        fn send(&self, req: Request) -> Result<Response> {
            let c = &self.0;
            let host = parse_target(&req.url, 0).unwrap().host;
            let current = c.now.fetch_add(1, Ordering::SeqCst) + 1;
            c.peak.fetch_max(current, Ordering::SeqCst);
            {
                let mut hosts = c.per_host.lock().unwrap();
                let entry = hosts.entry(host.clone()).or_default();
                entry.0 += 1;
                entry.1 = entry.1.max(entry.0);
            }
            std::thread::sleep(Duration::from_millis(15));
            c.per_host.lock().unwrap().get_mut(&host).unwrap().0 -= 1;
            c.now.fetch_sub(1, Ordering::SeqCst);
            Ok(Response::new(
                200,
                Vec::new(),
                req.url,
                Box::new(std::io::empty()),
            ))
        }
    }

    #[test]
    fn in_flight_requests_are_capped() {
        let counting = Arc::new(Counting::default());
        let limited = Limited::new(Box::new(Slow(Arc::clone(&counting))));
        std::thread::scope(|scope| {
            for i in 0..20 {
                let limited = &limited;
                scope.spawn(move || {
                    let host = if i % 2 == 0 {
                        "github.com"
                    } else {
                        "codeberg.org"
                    };
                    drop(
                        limited
                            .send(Request::get(format!("https://{host}/{i}"), "test"))
                            .unwrap(),
                    );
                });
            }
        });
        assert!(counting.peak.load(Ordering::SeqCst) <= MAX_CONCURRENT_REQUESTS);
        for (host, (_, peak)) in counting.per_host.lock().unwrap().iter() {
            assert!(*peak <= MAX_CONCURRENT_PER_HOST, "{host}: {peak}");
        }
    }

    #[test]
    fn a_body_holds_its_permits() {
        let limited = Limited::with_limits(Box::new(Slow(Arc::new(Counting::default()))), 1, 1);
        let held = limited
            .send(Request::get("https://github.com/a", "test"))
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let resp = limited
                    .send(Request::get("https://github.com/b", "test"))
                    .unwrap();
                tx.send(resp.status).unwrap();
            });
            assert!(
                rx.recv_timeout(Duration::from_millis(100)).is_err(),
                "blocked while the body lives"
            );
            drop(held);
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 200);
        });
    }
}
