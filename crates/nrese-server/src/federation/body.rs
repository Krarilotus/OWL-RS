//! Invocation-owned destination bytes. Incoming reqwest/hyper backing is a
//! separate, still-open receive boundary; this owner makes no claim about it.

use std::sync::Arc;

use nrese_exec::{Budget, ChargedVec, StopCheck};
use nrese_sparql::QueryEvaluationError;

pub(super) struct Body {
    bytes: ChargedVec<u8>,
    cap: Option<usize>,
}

impl Body {
    pub(super) fn new(budget: &Arc<Budget>, cap: Option<usize>) -> Self {
        Self {
            bytes: ChargedVec::new(Arc::clone(budget)),
            cap: cap.filter(|cap| *cap != 0),
        }
    }

    pub(super) fn declared(&self, length: Option<u64>) -> Result<(), QueryEvaluationError> {
        if self
            .cap
            .zip(length)
            .is_some_and(|(cap, length)| length > cap as u64)
        {
            return Err(self.too_long());
        }
        Ok(())
    }

    pub(super) fn append(
        &mut self,
        bytes: &[u8],
        stop: &dyn StopCheck,
    ) -> Result<(), QueryEvaluationError> {
        stop.check().map_err(QueryEvaluationError::from)?;
        let length = self.bytes.len().checked_add(bytes.len()).ok_or_else(|| {
            QueryEvaluationError::Unexpected(
                "SERVICE body length exceeds its representation".into(),
            )
        })?;
        if self.cap.is_some_and(|cap| length > cap) {
            return Err(self.too_long());
        }
        // ChargedVec reserves new capacity beside old capacity before copying.
        self.bytes.extend_from_slice(bytes)?;
        stop.check().map_err(QueryEvaluationError::from)?;
        Ok(())
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn too_long(&self) -> QueryEvaluationError {
        QueryEvaluationError::Service(
            format!(
                "response exceeds federation.max_response_bytes ({})",
                self.cap.unwrap()
            )
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_exec::{
        Stop, StopSource,
        measure::{alone, measure_within},
    };

    // The server lib-test target had no measured allocator. This is test-only;
    // without it an uncovered-allocation assertion would be vacuous.
    #[global_allocator]
    static MEASURED: nrese_exec::measure::Measured<std::alloc::System> =
        nrese_exec::measure::Measured(std::alloc::System);

    #[test]
    fn n7_body_cap_and_zero_cap_keep_actual_admission() {
        let budget = Arc::new(Budget::new(64));
        let mut body = Body::new(&budget, Some(3));
        assert!(body.declared(Some(4)).is_err());
        body.append(b"abc", &StopSource::none()).unwrap();
        assert_eq!(body.bytes(), b"abc");
        assert!(matches!(
            body.append(b"d", &StopSource::none()),
            Err(QueryEvaluationError::Service(_))
        ));
        assert_eq!(body.bytes(), b"abc");
        drop(body);
        assert_eq!(budget.used(), 0);

        let budget = Arc::new(Budget::new(0));
        let mut body = Body::new(&budget, Some(0));
        body.declared(Some(u64::MAX)).unwrap();
        assert!(matches!(
            body.append(b"x", &StopSource::none()),
            Err(QueryEvaluationError::MemoryLimit(_))
        ));
        assert!(body.bytes().is_empty());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn n7_body_growth_is_admitted_and_live_until_drop() {
        if !alone() {
            return;
        }
        let source = [7; 1000];
        let budget = Arc::new(Budget::new(1 << 20));
        let (result, uncovered) = measure_within(&budget, || {
            let mut body = Body::new(&budget, None);
            for _ in 0..17 {
                body.append(&source, &StopSource::none())?;
            }
            Ok::<_, QueryEvaluationError>(body)
        });
        assert_eq!(uncovered, 0);
        let body = result.unwrap();
        assert_eq!(body.bytes().len(), 17_000);
        assert!(budget.used() >= body.bytes().len());
        drop(body);
        assert_eq!(budget.used(), 0);

        // Growth must fund both the existing 1000 bytes and the new 2000-byte
        // block. A live-size-only charge would incorrectly accept this budget.
        let budget = Arc::new(Budget::new(2048));
        let ((body, error), uncovered) = measure_within(&budget, || {
            let mut body = Body::new(&budget, None);
            body.append(&source, &StopSource::none()).unwrap();
            let error = body.append(&source, &StopSource::none()).unwrap_err();
            (body, error)
        });
        assert_eq!(uncovered, 0);
        assert!(matches!(error, QueryEvaluationError::MemoryLimit(_)));
        assert_eq!(body.bytes().len(), 1000);
        drop(body);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn n7_body_stop_preserves_operational_reason_before_copy() {
        struct Stopped(Stop);
        impl StopCheck for Stopped {
            fn check(&self) -> Result<(), Stop> {
                Err(self.0)
            }
        }
        let budget = Arc::new(Budget::new(0));
        let refused = budget.charge(1).unwrap_err();
        for stop in [Stop::Cancelled, Stop::Memory(refused), Stop::Rows] {
            let mut body = Body::new(&budget, None);
            let error = body.append(b"ignored", &Stopped(stop)).unwrap_err();
            assert!(match stop {
                Stop::Cancelled => matches!(error, QueryEvaluationError::Cancelled),
                Stop::Memory(cause) =>
                    matches!(error, QueryEvaluationError::MemoryLimit(actual) if actual == cause),
                Stop::Rows => matches!(error, QueryEvaluationError::Unexpected(_)),
                Stop::TooLarge(_) => unreachable!(),
            });
            assert!(body.bytes().is_empty());
        }
        assert_eq!(budget.used(), 0);
    }
}
