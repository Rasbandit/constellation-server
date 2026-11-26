//! Independent units (one per player) on several threads, with a fixed merge order.

/// Items per thread for `workers` threads (at most `config::FILL_MAX_WORKERS`): each takes one
/// contiguous chunk.
fn per(len: usize, workers: usize) -> usize {
    len.div_ceil(workers.clamp(1, config::FILL_MAX_WORKERS)).max(1)
}

/// `f` over `items` on up to `workers` threads (at most `config::FILL_MAX_WORKERS`). The
/// result is in item order whatever the
/// worker count, because each worker takes one contiguous chunk and chunks are joined in order.
pub fn map_ordered<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let per = per(items.len(), workers);
    if items.len() <= per {
        return items.iter().map(f).collect();
    }
    std::thread::scope(|s| {
        let f = &f;
        let tasks: Vec<_> = items
            .chunks(per)
            .map(|chunk| s.spawn(move || chunk.iter().map(f).collect::<Vec<R>>()))
            .collect();
        tasks
            .into_iter()
            .flat_map(|t| t.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

/// `f` on every item in place, on up to `workers` threads (at most `config::FILL_MAX_WORKERS`).
/// Each item is touched by one thread and `f` sees nothing else, so the result does not depend
/// on the worker count. The caller's thread takes the first chunk.
pub fn for_each_mut<T: Send>(items: &mut [T], workers: usize, f: impl Fn(&mut T) + Sync) {
    let per = per(items.len(), workers);
    let (first, rest) = items.split_at_mut(per.min(items.len()));
    // A panic in a worker is raised again when the scope ends.
    std::thread::scope(|s| {
        let f = &f;
        for chunk in rest.chunks_mut(per) {
            s.spawn(move || chunk.iter_mut().for_each(f));
        }
        first.iter_mut().for_each(f);
    });
}

#[cfg(test)]
mod tests {
    use super::{for_each_mut, map_ordered};

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
