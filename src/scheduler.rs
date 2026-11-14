/// Execute the same per-item action in order. Stop on the first error or panic.
pub fn run_batch(
    indices: impl IntoIterator<Item = usize>,
    mut execute: impl FnMut(usize) -> Result<String, String>,
    mut report: impl FnMut(usize, Result<String, String>),
) {
    for index in indices {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(index)))
            .unwrap_or_else(|_| Err("后台操作发生异常".into()));
        let failed = result.is_err();
        report(index, result);
        if failed {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequential_batch_stops_after_first_error_or_panic() {
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
        assert_eq!(visited, [0]);
        assert_eq!(results, [(0, false)]);
        visited.clear();
        results.clear();
        run_batch(
            [1, 2, 3],
            |i| {
                visited.push(i);
                if i == 1 {
                    panic!("recoverable test fault");
                }
                Ok("done".into())
            },
            |i, result| results.push((i, result.is_ok())),
        );
        assert_eq!(visited, [1]);
        assert_eq!(results, [(1, false)]);
    }
}
