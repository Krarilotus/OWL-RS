//! Old consistency instances keyed by the witness name and projected schema bindings.

use nrese_exec::{Budget, ChargedMap, ChargedVec, Reservation, memory::MemoryStopCause};

use super::{GroundProgram, OperationStop, Poller, Source, Stopped, guards_hold, pattern};

type Substitution<'p> = &'p [Option<u64>];
type Projections<'p, 'm> = ChargedVec<Substitution<'p>, &'m Budget>;
const NO_INSTANCE: usize = usize::MAX;

/// The distinct schema projections per rule name and witness width, plus the old instances
/// under each full key. Several instances can share a key and differ in bodies or guards.
pub(crate) struct WitnessIndex<'p, 'm> {
    program: &'p GroundProgram,
    projections: ChargedMap<(&'p str, usize), Projections<'p, 'm>, Reservation<&'m Budget>>,
    heads: ChargedMap<(&'p str, Substitution<'p>), usize, Reservation<&'m Budget>>,
    next: ChargedVec<usize, &'m Budget>,
}

impl<'p, 'm> WitnessIndex<'p, 'm> {
    pub(crate) fn new(
        program: &'p GroundProgram,
        memory: &'m Budget,
        operation: &OperationStop<'_>,
        poll: &Poller<'_>,
    ) -> Result<Self, Stopped> {
        let refused = |_| {
            operation.refuse(MemoryStopCause::OverLimit);
            Stopped
        };
        let mut index = Self {
            program,
            projections: ChargedMap::new(memory),
            heads: ChargedMap::new(memory),
            next: ChargedVec::with_capacity(memory, program.consistency.len()).map_err(refused)?,
        };
        for (c, (rule, grounded)) in program.consistency.iter().enumerate() {
            if poll.tick() {
                return Err(Stopped);
            }
            #[cfg(test)]
            crate::witness::add(crate::witness::Site::OldWitnessInstance, 1);
            let name = rule.name.as_str();
            let substitution = grounded.substitution.as_slice();
            let (head, new_key) = index
                .heads
                .get_or_insert_with((name, substitution), || NO_INSTANCE)
                .map_err(refused)?;
            index
                .next
                .push(std::mem::replace(head, c))
                .map_err(refused)?;
            if new_key {
                let (layouts, _) = index
                    .projections
                    .get_or_insert_with((name, substitution.len()), || ChargedVec::new(memory))
                    .map_err(refused)?;
                let known = poll.counted(layouts.iter()).any(|layout| {
                    layout
                        .iter()
                        .zip(substitution)
                        .all(|(a, b)| a.is_some() == b.is_some())
                });
                poll.ended()?;
                if !known {
                    layouts.push(substitution).map_err(refused)?;
                }
            }
        }
        Ok(index)
    }

    /// The ground schema was already checked in the before-program. Probe the matching
    /// instances' remaining body atoms and guards in the before state.
    pub(crate) fn contains<S: Source + ?Sized>(
        &self,
        source: &S,
        name: &str,
        bindings: &[Option<u64>],
        poll: &Poller<'_>,
    ) -> Result<bool, Stopped> {
        let Some(layouts) = self.projections.get(&(name, bindings.len())) else {
            return Ok(false);
        };
        let mut scratch = [None; u8::MAX as usize + 1];
        let projected = &mut scratch[..bindings.len()];
        for layout in poll.counted(layouts.iter()) {
            for ((slot, value), bound) in projected.iter_mut().zip(bindings).zip(*layout) {
                *slot = if bound.is_some() { *value } else { None };
            }
            let mut candidate = self
                .heads
                .get(&(name, &*projected))
                .copied()
                .unwrap_or(NO_INSTANCE);
            'instances: while candidate != NO_INSTANCE {
                if poll.tick() {
                    return Err(Stopped);
                }
                #[cfg(test)]
                crate::witness::add(crate::witness::Site::OldWitnessInstance, 1);
                let grounded = &self.program.consistency[candidate].1;
                candidate = self.next[candidate];
                for atom in &grounded.rule.body {
                    let [Some(s), Some(p), Some(o)] = pattern(atom, bindings) else {
                        continue 'instances;
                    };
                    if !source.contains([s, p, o], poll)? {
                        continue 'instances;
                    }
                }
                if guards_hold(&grounded.rule.guards, bindings, poll) {
                    return Ok(true);
                }
            }
        }
        poll.ended()?;
        Ok(false)
    }
}
