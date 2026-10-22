//! Sidecars: small text files beside a durable file, `<file>.meta`, holding facts about it
//! that its format has no room for, as `key=value` lines (a WAL segment's sync policy).
//! Written atomically (a temporary file, published: [`super::publish`]) before the file
//! they describe; engines that don't know them ignore them, so they change no format.
//! A missing sidecar reads as no facts, and its readers fall back accordingly; one that is
//! there but can't be read (or names what this engine doesn't know) is an error.

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
    let text: String = entries
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect();
    write_text(file, &text)
}

/// Gives `to` the sidecar `from` has, written as [`write`] does; or none, removing a stale
/// one, if `from` has none. Before `to` itself is placed: a file never counts at its new
/// place without its facts.
pub(crate) fn copy(from: &Path, to: &Path) -> EngineResult<()> {
    match fs::read_to_string(path_of(from)) {
        Ok(text) => write_text(to, &text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::remove_file(path_of(to)) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
                _ => Ok(()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn write_text(file: &Path, text: &str) -> EngineResult<()> {
    let path = path_of(file);
    let mut tmp = OsString::from(path.as_os_str());
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    File::create(&tmp)?.write_all(text.as_bytes())?;
    super::publish::publish_or_fail(&tmp, &path)?;
    Ok(())
}

/// The value of `key` in the sidecar of `file`: `None` if there is no sidecar (or no such
/// key). A sidecar that is there but can't be read is an error, never taken for a missing
/// one.
pub(crate) fn read(file: &Path, key: &str) -> std::io::Result<Option<String>> {
    let text = match fs::read_to_string(path_of(file)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(text.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == key).then(|| value.to_owned())
    }))
}

/// Removes the sidecar of `file`, if any.
pub(crate) fn remove(file: &Path) {
    let _ = fs::remove_file(path_of(file));
}
