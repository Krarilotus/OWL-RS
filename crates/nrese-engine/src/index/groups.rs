//! Admitted parallel group parts and their sorted signed merge. No flattened group list.

use std::sync::atomic::{AtomicBool, Ordering};

use nrese_exec::{Budget, Reservation, Scratch, Stop, Ticker};
use parking_lot::Mutex;
use rayon::prelude::*;

use super::{GROUP_PART, IndexVersion, keys::BLOCK};
use crate::quad::AccessPlan;
use crate::term::TermId;

/// Arithmetic block-aligned cuts, including the old small-range single task.
pub(super) fn parts(start: usize, end: usize) -> usize {
    if start == end {
        0
    } else if end - start < 2 * GROUP_PART {
        1
    } else {
        end.div_ceil(GROUP_PART) - start / GROUP_PART
    }
}

pub(super) fn part(start: usize, end: usize, p: usize) -> (usize, usize) {
    if end - start < 2 * GROUP_PART {
        return (start, end);
    }
    let base = start / GROUP_PART;
    (
        if p == 0 {
            start
        } else {
            (base + p) * GROUP_PART
        },
        end.min((base + p + 1) * GROUP_PART),
    )
}

type Row = (u64, i64);

struct Part<'a> {
    rows: Scratch<'a, Row>,
    #[cfg(test)]
    units: u64,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Work {
    pub parts: usize,
    pub rows: usize,
    pub units: u64,
}

struct RunGroups<'a> {
    parts: Scratch<'a, Option<Part<'a>>>,
    part: usize,
    row: usize,
}

impl RunGroups<'_> {
    fn head(&self) -> Option<Row> {
        self.parts
            .get(self.part)?
            .as_ref()?
            .rows
            .get(self.row)
            .copied()
    }

    fn advance(&mut self) {
        self.row += 1;
        while let Some(Some(rows)) = self.parts.get(self.part) {
            if self.row < rows.rows.len() {
                break;
            }
            // A consumed part releases its payload immediately, not at the last run.
            self.parts[self.part] = None;
            self.part += 1;
            self.row = 0;
        }
    }
}

/// Payload precedes its token even when a fallible walk unwinds.
struct Column<'a> {
    values: Vec<u64>,
    _held: Reservation<&'a Budget>,
}

fn emit<W: FnMut(u32) -> Result<(), Stop>>(
    rows: &mut Scratch<'_, Row>,
    perm: &super::run::PermutationRun,
    value: u64,
    i: usize,
    j: usize,
    work: &mut W,
) -> Result<(), Stop> {
    // Each physical part bounds an opaque bitmap popcount to <=2048 words.
    let words = if perm.tombstones.is_empty() {
        0
    } else {
        j.div_ceil(64) - i / 64
    };
    work(1 + words as u32)?;
    rows.push((value, (j - i) as i64 - 2 * perm.tombstones_in(i, j) as i64))?;
    Ok(())
}

/// One head per run. Parts stay in their original order; equal values, including those
/// split at cuts, are signed-summed across all runs before visibility is decided.
pub(crate) struct Groups<'a, F: Fn() -> bool + Sync + ?Sized> {
    runs: Scratch<'a, RunGroups<'a>>,
    budget: &'a Budget,
    ticker: Ticker<&'a F>,
    done: bool,
    #[cfg(test)]
    pub(crate) work: Work,
}

impl<'a, F: Fn() -> bool + Sync + ?Sized> Groups<'a, F> {
    pub(crate) fn new(budget: &'a Budget, stop: &'a F, runs: usize) -> Result<Self, Stop> {
        let ticker = Ticker::new(stop)?;
        Ok(Self {
            runs: Scratch::with_capacity(budget, runs)?,
            budget,
            ticker,
            done: false,
            #[cfg(test)]
            work: Work::default(),
        })
    }

    pub(crate) fn prepare(
        &mut self,
        index: &IndexVersion,
        plan: &AccessPlan,
        position: usize,
        stop: &F,
    ) -> Result<(), Stop> {
        let Some(adapted) = index.layout.adapt(plan) else {
            return Ok(());
        };
        let graph = adapted.permutation != plan.permutation && position == 0;
        let position = if adapted.permutation == plan.permutation || graph {
            position
        } else {
            position - 1
        };
        for run in index.runs.iter() {
            self.ticker.check()?;
            let perm = run.permutation(adapted.permutation);
            let (start, end) = perm.range(&adapted.low, &adapted.high);
            self.ticker.check()?;
            let n = parts(start, end);
            if n == 0 {
                continue;
            }
            let mut slots = Scratch::with_capacity(self.budget, n)?;
            slots.fill(n, |room| {
                for _ in 0..n {
                    room.push(None);
                }
            })?;
            let failed = AtomicBool::new(false);
            let error = Mutex::new(None);
            let check = || failed.load(Ordering::Relaxed) || stop();
            let walk = |p| -> Result<_, Stop> {
                let mut ticker = Ticker::new(&check)?;
                let (a, b) = part(start, end, p);
                let mut rows = Scratch::new(self.budget);
                #[cfg(test)]
                let mut units = 0u64;
                let mut work = |n: u32| {
                    #[cfg(test)]
                    {
                        units += u64::from(n);
                    }
                    ticker.tick(n)
                };
                if graph {
                    emit(
                        &mut rows,
                        perm,
                        TermId::DEFAULT_GRAPH.raw(),
                        a,
                        b,
                        &mut work,
                    )?;
                } else {
                    let (values, held) = Scratch::with_capacity(self.budget, BLOCK)?.into_parts();
                    let mut column = Column {
                        values,
                        _held: held,
                    };
                    perm.keys.try_groups(
                        a,
                        b,
                        position,
                        &mut column.values,
                        &mut work,
                        |v, i, j, work| emit(&mut rows, perm, v, i, j, work),
                    )?;
                }
                ticker.finish()?;
                Ok(Part {
                    rows,
                    #[cfg(test)]
                    units,
                })
            };
            if n == 1 {
                slots[0] = Some(walk(0)?);
            } else {
                // All running tasks join on failure. The flag is read at checkpoints,
                // never per key; the first real failure is retained over peer stops.
                slots
                    .par_iter_mut()
                    .enumerate()
                    .try_for_each(|(p, slot)| match walk(p) {
                        Ok(rows) => {
                            *slot = Some(rows);
                            Ok(())
                        }
                        Err(e) => {
                            let mut first = error.lock();
                            first.get_or_insert(e);
                            failed.store(true, Ordering::Relaxed);
                            Err(())
                        }
                    })
                    .ok();
                if let Some(e) = error.into_inner() {
                    return Err(e);
                }
            }
            #[cfg(test)]
            {
                self.work.parts += n;
                for part in slots.iter().flatten() {
                    self.work.rows += part.rows.len();
                    self.work.units += part.units;
                }
            }
            self.runs.push(RunGroups {
                parts: slots,
                part: 0,
                row: 0,
            })?;
        }
        self.ticker.finish()
    }

    pub(crate) fn discard(&mut self) {
        self.runs = Scratch::new(self.budget);
        self.done = true;
    }

    fn next_group(&mut self) -> Result<Option<(TermId, u64)>, Stop> {
        loop {
            let mut value = None;
            for run in self.runs.iter() {
                self.ticker.tick(1)?;
                if let Some((v, _)) = run.head() {
                    value = Some(value.map_or(v, |old: u64| old.min(v)));
                }
            }
            let Some(value) = value else {
                self.ticker.finish()?;
                return Ok(None);
            };
            let mut total = 0i128;
            for run in self.runs.iter_mut() {
                while let Some((v, count)) = run.head() {
                    self.ticker.tick(1)?;
                    if v != value {
                        break;
                    }
                    total += i128::from(count);
                    run.advance();
                }
            }
            if total > 0 {
                let count = u64::try_from(total).map_err(|_| {
                    Stop::TooLarge(nrese_exec::narrow::RepresentationLimit {
                        what: "a group count",
                        limit: u64::MAX,
                    })
                })?;
                return Ok(Some((TermId::from_raw(value), count)));
            }
        }
    }
}

impl<F: Fn() -> bool + Sync + ?Sized> Iterator for Groups<'_, F> {
    type Item = Result<(TermId, u64), Stop>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_group() {
            Ok(Some(row)) => Some(Ok(row)),
            result => {
                self.discard();
                result.err().map(Err)
            }
        }
    }
}
