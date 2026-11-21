use std::collections::{HashSet, VecDeque};
use std::fmt::Debug;
use std::hash::{BuildHasherDefault, DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

pub trait Model {
    type World: Clone + Eq + Hash + Debug;
    type Move: Clone + Debug;
    const DRAIN_BOUND: usize;

    fn start() -> Self::World;
    fn moves(world: &Self::World) -> Vec<Self::Move>;
    fn apply(world: &Self::World, action: &Self::Move) -> Self::World;
    fn check(world: &Self::World) -> Result<(), String>;
    fn quiet(world: &Self::World) -> bool;

    // A legal server move toward quiet, independent of the search's stimulus budget.
    fn drain_move(world: &Self::World) -> Option<Self::Move>;
    fn answer(world: &Self::World, action: &Self::Move, next: &Self::World) -> String;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub worlds: usize,
    pub completed_depth: usize,
    pub budget_hit: bool,
    pub drain_time: Duration,
}

impl Stats {
    pub fn collision_bound(self) -> f64 {
        let n = self.worlds as f64;
        n * (n - 1.0) / 2_f64.powi(65)
    }
}

#[derive(Debug)]
pub struct Trace<M> {
    pub violation: String,
    pub schedule: Vec<(M, String)>,
    pub drain: Vec<(M, String)>,
    pub stats: Stats,
}

// The frontier shares the parent's replay path for all newly discovered siblings.
const MOVE_BITS: usize = 7;
const MOVES: usize = 1 << MOVE_BITS;
type Path = u128;

struct Batch {
    parent: Path,
    children: u128,
}

fn child_path(parent: Path, parent_depth: usize, index: usize) -> Path {
    parent | ((index as Path) << (parent_depth * MOVE_BITS))
}

fn replay<M: Model>(
    mut world: M::World,
    mut path: Path,
    depth: usize,
    mut record: impl FnMut(&M::World, M::Move, &M::World),
) -> M::World {
    for _ in 0..depth {
        let index = (path as usize) & (MOVES - 1);
        let action = M::moves(&world).swap_remove(index);
        let next = M::apply(&world, &action);
        record(&world, action, &next);
        world = next;
        path >>= MOVE_BITS;
    }
    world
}

fn fingerprint<W: Hash>(world: &W) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write_u64(0x4e52_4553_452d_5458);
    world.hash(&mut hasher);
    hasher.finish()
}

fn drain<M: Model>(world: &M::World) -> Result<(), (String, usize)> {
    if M::quiet(world) {
        return Ok(());
    }
    let mut at = world.clone();
    for taken in 0..M::DRAIN_BOUND {
        let Some(action) = M::drain_move(&at) else {
            return Err(("server drain has no enabled move".into(), taken));
        };
        at = M::apply(&at, &action);
        M::check(&at).map_err(|why| (why, taken + 1))?;
        if M::quiet(&at) {
            return Ok(());
        }
    }
    Err((
        format!("server drain exceeded {} moves", M::DRAIN_BOUND),
        M::DRAIN_BOUND,
    ))
}

struct Search {
    seen: HashSet<u64, BuildHasherDefault<DefaultHasher>>,
    max_worlds: usize,
    stats: Stats,
}

impl Search {
    fn failure<M: Model>(
        &self,
        path: Path,
        depth: usize,
        violation: String,
        drain_steps: usize,
    ) -> Trace<M::Move> {
        let mut schedule = Vec::new();
        let mut world = replay::<M>(M::start(), path, depth, |before, action, after| {
            let answer = M::answer(before, &action, after);
            schedule.push((action, answer));
        });
        let mut drain = Vec::new();
        for _ in 0..drain_steps {
            let action = M::drain_move(&world).expect("replay the failing drain");
            let next = M::apply(&world, &action);
            drain.push((action.clone(), M::answer(&world, &action, &next)));
            world = next;
        }
        Trace {
            violation,
            schedule,
            drain,
            stats: self.stats,
        }
    }

    fn check_new<M: Model>(
        &mut self,
        world: &M::World,
        path: Path,
        depth: usize,
    ) -> Result<(), Trace<M::Move>> {
        self.stats.worlds = self.seen.len();
        M::check(world).map_err(|why| self.failure::<M>(path, depth, why, 0))?;
        let start = Instant::now();
        let result = drain::<M>(world);
        self.stats.drain_time += start.elapsed();
        result
            .map_err(|(why, steps)| self.failure::<M>(path, depth, format!("drain: {why}"), steps))
    }

    fn expand<M: Model>(
        &mut self,
        world: &M::World,
        parent: Path,
        depth: usize,
        frontier: &mut VecDeque<Batch>,
    ) -> Result<(), Trace<M::Move>> {
        let moves = M::moves(world);
        if moves.len() > MOVES {
            return Err(self.failure::<M>(
                parent,
                depth,
                "too many moves to encode the frontier".into(),
                0,
            ));
        }
        let mut children = 0;
        for (index, action) in moves.iter().enumerate() {
            let next = M::apply(world, action);
            let fingerprint = fingerprint(&next);
            if self.seen.len() == self.max_worlds {
                if self.seen.contains(&fingerprint) {
                    continue;
                }
                self.stats.budget_hit = true;
                return Ok(());
            }
            if self.seen.insert(fingerprint) {
                self.check_new::<M>(&next, child_path(parent, depth, index), depth + 1)?;
                children |= 1_u128 << index;
            }
        }
        if children != 0 {
            frontier.push_back(Batch { parent, children });
        }
        Ok(())
    }
}

pub fn explore<M: Model>(depth: usize, max_worlds: usize) -> Result<Stats, Trace<M::Move>> {
    assert!(max_worlds > 0);
    assert!(depth <= Path::BITS as usize / MOVE_BITS);
    let mut search = Search {
        seen: HashSet::with_capacity_and_hasher(max_worlds, BuildHasherDefault::default()),
        max_worlds,
        stats: Stats::default(),
    };
    let start = M::start();
    search.seen.insert(fingerprint(&start));
    search.check_new::<M>(&start, 0, 0)?;
    if depth == 0 {
        return Ok(search.stats);
    }

    let mut frontier = VecDeque::new();
    search.expand::<M>(&start, 0, 0, &mut frontier)?;
    'layers: for level in 1..=depth {
        if search.stats.budget_hit {
            break;
        }
        search.stats.completed_depth = level;
        if level == depth || frontier.is_empty() {
            if frontier.is_empty() {
                search.stats.completed_depth = depth;
            }
            break;
        }
        for _ in 0..frontier.len() {
            let Batch {
                parent,
                mut children,
            } = frontier.pop_front().expect("current layer");
            let world = replay::<M>(start.clone(), parent, level - 1, |_, _, _| {});
            let moves = M::moves(&world);
            while children != 0 {
                let index = children.trailing_zeros() as usize;
                children &= children - 1;
                let next = M::apply(&world, &moves[index]);
                let path = child_path(parent, level - 1, index);
                search.expand::<M>(&next, path, level, &mut frontier)?;
                if search.stats.budget_hit {
                    break 'layers;
                }
            }
        }
    }
    Ok(search.stats)
}

#[cfg(test)]
mod tests {
    use super::{Model, explore};

    // Two routes converge on the same layer. The last vertex can be made bad.
    struct Graph<const BAD: bool>;

    impl<const BAD: bool> Model for Graph<BAD> {
        type World = u8;
        type Move = u8;
        const DRAIN_BOUND: usize = 0;

        fn start() -> u8 {
            0
        }

        fn moves(world: &u8) -> Vec<u8> {
            match world {
                0 => vec![1, 2],
                1 | 2 => vec![3, 4],
                3 => vec![5],
                _ => vec![],
            }
        }

        fn apply(_world: &u8, edge: &u8) -> u8 {
            *edge
        }

        fn check(world: &u8) -> Result<(), String> {
            if BAD && *world == 5 {
                return Err("reached vertex 5".into());
            }
            Ok(())
        }

        fn quiet(_world: &u8) -> bool {
            true
        }

        fn drain_move(_world: &u8) -> Option<u8> {
            None
        }

        fn answer(world: &u8, _edge: &u8, next: &u8) -> String {
            format!("{world} -> {next}")
        }
    }

    // A reachable failure carries the full shortest schedule and each answer.
    #[test]
    fn finds_bad_vertex_with_its_schedule() {
        let trace = explore::<Graph<true>>(3, 10).unwrap_err();
        assert_eq!(trace.violation, "reached vertex 5");
        assert_eq!(
            trace.schedule,
            [
                (1, "0 -> 1".to_owned()),
                (3, "1 -> 3".to_owned()),
                (5, "3 -> 5".to_owned()),
            ]
        );
        assert!(trace.drain.is_empty());
    }

    // The four layers contain 1, 2, 2, 1 unique vertices, then no new vertices.
    #[test]
    fn counts_unique_vertices_by_complete_layer() {
        for (depth, worlds) in [(0, 1), (1, 3), (2, 5), (3, 6), (8, 6)] {
            let stats = explore::<Graph<false>>(depth, 6).unwrap();
            assert_eq!((stats.completed_depth, stats.worlds), (depth, worlds));
            assert!(!stats.budget_hit);
        }
    }

    // A cap reached partway through layer two leaves only layer one complete.
    #[test]
    fn budget_reports_the_last_complete_layer() {
        let stats = explore::<Graph<false>>(3, 4).unwrap();
        assert_eq!((stats.completed_depth, stats.worlds), (1, 4));
        assert!(stats.budget_hit);
    }
}
