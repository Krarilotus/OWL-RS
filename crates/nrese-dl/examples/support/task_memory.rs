//! `--task-memory-mib N`, shared by the examples: the context saturation's capacity
//! envelope in MiB (0: none).

pub fn task_memory(mib: u64) -> Result<Option<usize>, String> {
    if mib == 0 {
        return Ok(None);
    }
    usize::try_from(mib)
        .ok()
        .and_then(|mib| mib.checked_mul(1 << 20))
        .map(Some)
        .ok_or_else(|| "memory budget overflow".into())
}
