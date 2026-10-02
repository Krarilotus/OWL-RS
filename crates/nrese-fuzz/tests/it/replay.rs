/// Replays the inputs in `NRESE_FUZZ_REPLAY` (a directory of `<target>-<hash>.bin` files).
#[test]
#[ignore = "replays saved findings"]
fn replay_findings() {
    let Ok(dir) = std::env::var("NRESE_FUZZ_REPLAY") else {
        return;
    };
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let target = nrese_fuzz::Target::ALL
            .into_iter()
            .find(|t| name.starts_with(&format!("{}-", t.name())))
            .unwrap();
        eprintln!("== {name}");
        let data = std::fs::read(entry.path()).unwrap();
        let _ = std::panic::catch_unwind(|| target.run(&data));
    }
}
