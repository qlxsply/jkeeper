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
