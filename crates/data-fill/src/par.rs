//! Parallel setup of independent units (one per player) with a fixed merge order.

/// `f` over `items` on up to `workers` threads (at most `config::FILL_MAX_WORKERS`). The
/// result is in item order whatever the
/// worker count, because each worker takes one contiguous chunk and chunks are joined in order.
pub fn map_ordered<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let workers = workers.clamp(1, config::FILL_MAX_WORKERS);
    let per = items.len().div_ceil(workers).max(1);
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

#[cfg(test)]
mod tests {
    use super::map_ordered;

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
