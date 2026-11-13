/// A submitted batch owns the task slot for its entire duration. Each item is
/// isolated so an ordinary error or unwind cannot discard the remaining items.
pub fn run_batch(
    indices: impl IntoIterator<Item = usize>,
    mut execute: impl FnMut(usize) -> Result<String, String>,
    mut report: impl FnMut(usize, Result<String, String>),
) {
    for index in indices {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(index)))
            .unwrap_or_else(|_| Err("后台操作发生异常，已跳过当前应用".into()));
        report(index, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequential_batch_continues_after_error_and_panic() {
        let mut visited = vec![];
        let mut results = vec![];
        run_batch(
            [0, 1, 2, 3],
            |i| {
                visited.push(i);
                match i {
                    0 => Err("missing configuration".into()),
                    1 => panic!("recoverable test fault"),
                    _ => Ok("done".into()),
                }
            },
            |i, result| results.push((i, result.is_ok())),
        );
        assert_eq!(visited, [0, 1, 2, 3]);
        assert_eq!(results, [(0, false), (1, false), (2, true), (3, true)]);
    }
}
