//! `run_bounded`: a fixed number of scoped worker threads pulling from a shared queue.
//! Sources fetch many refs with it (`MAX_CONCURRENT_REQUESTS` workers); `Limited`
//! still caps requests per host. Results come back in input order (DEC-3).

use std::collections::VecDeque;
use std::sync::Mutex;

/// `f` applied to every item by at most `max_workers` threads at once; results in
/// input order.
pub fn run_bounded<T, R>(items: Vec<T>, max_workers: usize, f: impl Fn(T) -> R + Sync) -> Vec<R>
where
    T: Send,
    R: Send,
{
    let count = items.len();
    let queue: Mutex<VecDeque<(usize, T)>> = Mutex::new(items.into_iter().enumerate().collect());
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..count).map(|_| None).collect());
    let workers = max_workers.clamp(1, count.max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let next = queue
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .pop_front();
                    let Some((index, item)) = next else { break };
                    let result = f(item);
                    if let Some(slot) = results
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get_mut(index)
                    {
                        *slot = Some(result);
                    }
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .into_iter()
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn order_is_kept_and_workers_are_bounded() {
        let now = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let out = run_bounded((0..40).collect(), 4, |i: u64| {
            let current = now.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(current, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(2 + (i % 3)));
            now.fetch_sub(1, Ordering::SeqCst);
            i * 10
        });
        assert_eq!(out, (0..40).map(|i| i * 10).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) <= 4);
        assert!(run_bounded(Vec::<u8>::new(), 6, |x| x).is_empty());
        assert_eq!(
            run_bounded(vec![1, 2], 0, |x| x + 1),
            [2, 3],
            "at least one worker"
        );
    }
}
