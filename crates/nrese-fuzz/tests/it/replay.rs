/// Replays the inputs in `NRESE_FUZZ_REPLAY` (a directory of `<target>-<hash>.bin` files)
/// on the campaign's stack; fails with the inputs that still panic, and when the directory
/// holds none to replay.
#[test]
#[ignore = "replays saved findings"]
fn replay_findings() {
    let Ok(dir) = std::env::var("NRESE_FUZZ_REPLAY") else {
        return;
    };
    let inputs: Vec<(nrese_fuzz::Target, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let target = nrese_fuzz::Target::ALL
                .into_iter()
                .find(|t| name.starts_with(&format!("{}-", t.name())) && name.ends_with(".bin"))?;
            Some((target, entry.path()))
        })
        .collect();
    assert!(!inputs.is_empty(), "no saved inputs to replay in {dir}");
    let failures: Vec<String> = inputs
        .into_iter()
        .filter_map(|(target, path)| {
            eprintln!("== {}", path.display());
            let data = std::fs::read(&path).unwrap();
            let held = std::thread::Builder::new()
                .stack_size(super::STACK)
                .spawn(move || std::panic::catch_unwind(|| target.run(&data)).is_ok())
                .expect("a thread")
                .join()
                .expect("the replay's thread");
            (!held).then(|| path.display().to_string())
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} saved inputs still fail:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
