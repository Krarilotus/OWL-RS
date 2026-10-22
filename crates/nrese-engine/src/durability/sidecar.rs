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

pub(crate) fn write_text(file: &Path, text: &str) -> EngineResult<()> {
    let path = path_of(file);
    let mut tmp = OsString::from(path.as_os_str());
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    File::create(&tmp)?.write_all(text.as_bytes())?;
    super::publish::publish_or_fail(&tmp, &path)?;
    Ok(())
}

/// A value from one captured sidecar snapshot; missing keys remain absent.
pub(crate) fn value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == key).then_some(value)
    })
}

pub(crate) fn read_text(file: &Path) -> std::io::Result<Option<String>> {
    match fs::read_to_string(path_of(file)) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Removes the sidecar of `file`, if any.
pub(crate) fn remove(file: &Path) {
    let _ = fs::remove_file(path_of(file));
}
