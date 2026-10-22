//! The commit clock and whether the entire retained history has known source times.
//! Unknown provenance is sticky, independently of the monotonic logical clock.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommitTime {
    pub(crate) clock: u64,
    pub(crate) known: bool,
}

impl CommitTime {
    pub(crate) const fn fresh() -> Self {
        Self {
            clock: 0,
            known: true,
        }
    }

    pub(crate) const fn unknown() -> Self {
        Self {
            clock: 0,
            known: false,
        }
    }

    /// O(1); a new timed record never repairs an unknown prefix.
    pub(crate) fn observe(self, time: Option<u64>) -> Self {
        Self {
            clock: self.clock.max(time.unwrap_or(0)),
            known: self.known && time.is_some_and(|time| time > 0),
        }
    }

    pub(crate) fn local(self) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |since| since.as_micros() as u64)
            .max(1);
        Self {
            clock: self.clock.max(now),
            ..self
        }
    }

    /// A physical WAL marker retains the clock floor, never semantic provenance.
    pub(crate) fn with_floor(self, floor: u64) -> Self {
        Self {
            clock: self.clock.max(floor),
            ..self
        }
    }

    pub(crate) fn check_cut(self, until: u64) -> crate::error::EngineResult<()> {
        if !self.known || self.clock > until {
            return Err(crate::EngineError::Configuration(
                "time restore needs a known checkpoint/history no newer than its cutoff; use revision restore instead".into(),
            ));
        }
        Ok(())
    }
}
