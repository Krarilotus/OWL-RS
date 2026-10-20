//! Sidecars: small text files beside a durable file, `<file>.meta`, holding facts about it
//! that its format has no room for, as `key=value` lines (a WAL segment's sync policy).
//! Written atomically (a temporary file, synced, renamed, the directory synced) before the
//! file they describe; engines that don't know them ignore them, so they change no format.
//! A missing or unreadable sidecar reads as no facts: its readers fall back accordingly.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::EngineResult;

/// The sidecar of `file`.
pub(crate) fn path_of(file: &Path) -> PathBuf {
    let mut name = OsString::from(file.as_os_str());
    name.push(".meta");
    PathBuf::from(name)
}

/// Writes the sidecar of `file`, replacing any.
pub(crate) fn write(file: &Path, entries: &[(&str, &str)]) -> EngineResult<()> {
    let path = path_of(file);
    let mut tmp = OsString::from(path.as_os_str());
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let text: String = entries
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect();
    let mut out = File::create(&tmp)?;
    out.write_all(text.as_bytes())?;
    out.sync_all()?;
    drop(out);
    fs::rename(&tmp, &path)?;
    if let Some(dir) = path.parent() {
        super::sync_dir(dir)?;
    }
    Ok(())
}

/// The value of `key` in the sidecar of `file`, if there is one.
pub(crate) fn read(file: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(path_of(file)).ok()?;
    text.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == key).then(|| value.to_owned())
    })
}

/// Removes the sidecar of `file`, if any.
pub(crate) fn remove(file: &Path) {
    let _ = fs::remove_file(path_of(file));
}
