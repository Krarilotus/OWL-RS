//! Format 12 refuses older binary stores: operators must export and reload into a new
//! directory. Exercise the public server commands with the original main-store fixture
//! (written by main at d9a4b24), preserving its folder and image bytes.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[test]
fn old_binary_stores_need_export_and_reload() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/main-store");
    for (name, restore) in [("folder", false), ("image", true)] {
        let dir = tempfile::tempdir()?;
        let data = dir.path().join("data");
        let config = dir.path().join("config.toml");
        let data_path = data.display().to_string().replace('\\', "/");
        std::fs::write(
            &config,
            format!(
                "[server]\nbind_addr = \"127.0.0.1:0\"\n\
                 deployment_posture = \"internal-authenticated\"\n\
                 [store]\nmode = \"on-disk\"\ndata_dir = \"{data_path}\"\nwal_archive = true\n\
                 [reasoner]\nmode = \"owl2-rl\"\n\
                 [auth]\nmode = \"bearer-static\"\n\
                 [auth.bearer_static]\nadmin_token = \"fixture-admin\"\n\
                 read_token = \"fixture-read\"\n"
            ),
        )?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_nrese-server"));
        if restore {
            let backup = dir.path().join("backup");
            copy_dir(&fixture.join("backup"), &backup)?;
            // The CLI uses restore_until, which opens the copied image before returning;
            // restore_image alone only places bytes and does not own format validation.
            command.arg("restore").arg(backup);
        } else {
            copy_dir(&fixture.join("data"), &data)?;
        }
        let stderr_path = dir.path().join("stderr");
        let mut child = command
            .arg("--config")
            .arg(config)
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr_path)?)
            .spawn()?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() < Duration::from_secs(10) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Err(error) = result {
                        return Err(error.into());
                    }
                    let stderr = std::fs::read_to_string(&stderr_path)?;
                    return Err(format!("{name}: no refusal within 10 s: {stderr}").into());
                }
            }
        };
        let stderr = std::fs::read_to_string(&stderr_path)?;
        assert!(!status.success(), "{name}: {status}: {stderr}");
        assert!(
            stderr.contains("unsupported storage format")
                && stderr.contains("reload the data into a new directory"),
            "{name}: {stderr}"
        );
    }
    Ok(())
}
