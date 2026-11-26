//! Independent units (one per player) on several threads, with a fixed merge order.

use std::sync::Mutex;

/// Items per thread for `workers` threads (at most `config::FILL_MAX_WORKERS`): each takes one
/// contiguous chunk.
fn per(len: usize, workers: usize) -> usize {
    len.div_ceil(workers.clamp(1, config::FILL_MAX_WORKERS)).max(1)
}

/// `f` over `items` on up to `workers` threads (at most `config::FILL_MAX_WORKERS`). The
/// result is in item order whatever the worker count: each result lands in its item's slot.
pub fn map_ordered<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let mut slots: Vec<(&T, Option<R>)> = items.iter().map(|x| (x, None)).collect();
    for_each_mut(&mut slots, workers, |(x, r)| *r = Some(f(x)));
    slots.into_iter().filter_map(|(_, r)| r).collect()
}

/// `f` on every item in place, on up to `workers` threads (at most `config::FILL_MAX_WORKERS`).
/// Each item is touched by one thread and `f` sees nothing else, so the result does not depend
/// on the worker count, nor on how many threads the system gives: the caller's thread takes the
/// first chunk and then every chunk no thread has taken, so a thread that cannot be started
/// (the process is at a cap) costs time, never the result.
pub fn for_each_mut<T: Send>(items: &mut [T], workers: usize, f: impl Fn(&mut T) + Sync) {
    let per = per(items.len(), workers);
    let (first, rest) = items.split_at_mut(per.min(items.len()));
    if rest.is_empty() {
        return first.iter_mut().for_each(f);
    }
    // A chunk waits in its slot for whoever takes it first.
    let slots: Vec<Mutex<Option<&mut [T]>>> =
        rest.chunks_mut(per).map(|c| Mutex::new(Some(c))).collect();
    let run = |slot: &Mutex<Option<&mut [T]>>| {
        let chunk = slot.lock().ok().and_then(|mut c| c.take());
        chunk.into_iter().flatten().for_each(&f);
    };
    // A panic in a worker is raised again when the scope ends.
    std::thread::scope(|s| {
        let run = &run;
        for slot in &slots {
            if !refused() {
                // An error leaves the chunk in its slot for the caller's thread below.
                let _ = std::thread::Builder::new().spawn_scoped(s, move || run(slot));
            }
        }
        first.iter_mut().for_each(&f);
        slots.iter().for_each(run);
    });
}

#[cfg(test)]
thread_local! {
    /// Tests: behave as if the system gave this thread no further threads.
    static REFUSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether to skip starting threads (only ever true in this module's tests).
fn refused() -> bool {
    #[cfg(test)]
    return REFUSE.with(std::cell::Cell::get);
    #[cfg(not(test))]
    false
}

#[cfg(test)]
mod tests {
    use super::{REFUSE, for_each_mut, map_ordered};

    #[test]
    fn every_item_is_changed_once_whatever_the_worker_count() {
        for workers in [0, 1, 2, 3, 8, 64] {
            let mut items: Vec<u32> = (0..23).collect();
            for_each_mut(&mut items, workers, |x| *x = *x * *x + 1);
            let want: Vec<u32> = (0..23).map(|x| x * x + 1).collect();
            assert_eq!(items, want, "{workers} workers");
        }
        for_each_mut::<u32>(&mut [], 4, |x| *x += 1);
    }

    /// With no thread to be had the work is still done, all of it on the caller's thread.
    #[test]
    fn a_thread_that_cannot_start_leaves_its_chunk_to_the_caller() {
        REFUSE.with(|r| r.set(true));
        let here = std::thread::current().id();
        let mut items: Vec<(u32, bool)> = (0..23).map(|x| (x, false)).collect();
        for_each_mut(&mut items, 4, |x| *x = (x.0 + 1, std::thread::current().id() == here));
        let want: Vec<(u32, bool)> = (1..24).map(|x| (x, true)).collect();
        assert_eq!(items, want);
        assert_eq!(map_ordered(&[1u32, 2, 3, 4, 5], 3, |x| x * 2), vec![2, 4, 6, 8, 10]);
        REFUSE.with(|r| r.set(false));
    }

    #[test]
    fn the_result_order_does_not_depend_on_the_worker_count() {
        let items: Vec<u32> = (0..23).collect();
        let want: Vec<u32> = items.iter().map(|x| x * x).collect();
        for workers in [0, 1, 2, 3, 8, 64] {
            assert_eq!(map_ordered(&items, workers, |x| x * x), want, "{workers} workers");
        }
        assert!(map_ordered::<u32, u32>(&[], 4, |x| *x).is_empty());
    }
}
