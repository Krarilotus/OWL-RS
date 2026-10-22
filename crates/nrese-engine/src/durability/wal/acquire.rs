//! Capture a segment with a stable metadata snapshot, including external prune/copy.
//! Metadata is published before replacement bytes and nonempty facts never heal. A
//! changed snapshot therefore invalidates acquisition; a later unlink cannot alter
//! an already captured file/fact pair. Missing throughout still means Unknown.

use std::fs::File;
use std::io;
use std::path::Path;

use super::{EngineError, EngineResult, SegmentFacts, SyncPolicy, sidecar, time};

impl SegmentFacts {
    pub(super) fn from_text(path: &Path, text: io::Result<Option<String>>) -> Self {
        let time = time::from_text(path, text.as_ref().ok().and_then(|text| text.as_deref()));
        let policy = text.map_err(EngineError::from).and_then(|text| {
            match text
                .as_deref()
                .and_then(|text| sidecar::value(text, "sync"))
            {
                None => Ok(None),
                Some(word) => SyncPolicy::from_word(word).map(Some).ok_or_else(|| {
                    EngineError::Corruption(format!(
                        "{}: unknown sync policy {word:?}",
                        sidecar::path_of(path).display()
                    ))
                }),
            }
        });
        Self { time, policy }
    }
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "WAL metadata changed during acquisition",
    )
}

fn capture(path: &Path) -> EngineResult<(File, io::Result<Option<String>>)> {
    let before = sidecar::read_text(path);
    let file = File::open(path)?;
    #[cfg(test)]
    after_open(path);
    let after = sidecar::read_text(path);
    if let (Ok(before), Ok(after)) = (&before, &after)
        && before != after
    {
        return Err(changed().into());
    }
    // An unreadable policy remains deferred until damaged bytes need classification.
    Ok((file, after))
}

pub(crate) fn open_segment(path: &Path) -> EngineResult<(File, SegmentFacts)> {
    let (file, text) = capture(path)?;
    Ok((file, SegmentFacts::from_text(path, text)))
}

pub(super) fn copy_segment(from: &Path, partial: &Path) -> EngineResult<Option<String>> {
    let (mut file, text) = capture(from)?;
    // Copy has always required readable metadata before publishing its destination.
    let text = text?;
    let mut target = File::create(partial)?;
    io::copy(&mut file, &mut target)?;
    target.set_permissions(file.metadata()?.permissions())?;
    if sidecar::read_text(from)? != text {
        return Err(changed().into());
    }
    // An empty Known source can publish Unknown before its first append while copy
    // reads it. Revalidation above prevents that old fact certifying the new bytes.
    Ok(text)
}

#[cfg(test)]
type AfterOpen = Box<dyn FnMut(&Path)>;

#[cfg(test)]
thread_local! {
    static AFTER_OPEN: std::cell::RefCell<Option<AfterOpen>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn after_open(path: &Path) {
    AFTER_OPEN.with(|slot| {
        // A replacement may itself copy a file; it must not recursively run this seam.
        let action = slot.borrow_mut().take();
        if let Some(mut action) = action {
            action(path);
            *slot.borrow_mut() = Some(action);
        }
    });
}

#[cfg(test)]
pub(crate) fn on_open(action: Option<AfterOpen>) {
    AFTER_OPEN.with(|slot| *slot.borrow_mut() = action);
}
