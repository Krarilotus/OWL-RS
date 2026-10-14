//! Static working-set layout. The solver is compiled once for each pair type; width is
//! selected at the operation boundary, never while a pair or candidate is visited.

use crate::eval::{Candidate, Pso};
use crate::ir::Triple;
use nrese_engine::PairChunks;
use std::sync::Arc;

pub(crate) trait Representation:
    Copy + Ord + Default + std::fmt::Debug + Send + Sync + 'static
{
    type Candidate: Candidate;
    fn pair(a: u64, b: u64) -> Self;
    fn first(self) -> u64;
    fn second(self) -> u64;
    fn swapped(self) -> Self;
    fn candidate(c: Self::Candidate) -> (u64, Self);
    fn export(chunks: Vec<Vec<Self>>) -> PairChunks;
}

impl Representation for u64 {
    type Candidate = u128;
    #[inline]
    fn pair(a: u64, b: u64) -> Self {
        debug_assert!((a | b) >> 32 == 0, "checked dense ranks");
        (a << 32) | b
    }
    #[inline]
    fn first(self) -> u64 {
        self >> 32
    }
    #[inline]
    fn second(self) -> u64 {
        self & 0xFFFF_FFFF
    }
    #[inline]
    fn swapped(self) -> Self {
        self.rotate_left(32)
    }
    #[inline]
    fn candidate(c: u128) -> (u64, Self) {
        ((c >> 64) as u64, c as u64)
    }
    fn export(chunks: Vec<Vec<Self>>) -> PairChunks {
        PairChunks::Numbers {
            chunks,
            ids: Arc::from([]),
        }
    }
}

impl Representation for [u64; 2] {
    type Candidate = Pso;
    #[inline]
    fn pair(a: u64, b: u64) -> Self {
        [a, b]
    }
    #[inline]
    fn first(self) -> u64 {
        self[0]
    }
    #[inline]
    fn second(self) -> u64 {
        self[1]
    }
    #[inline]
    fn swapped(self) -> Self {
        [self[1], self[0]]
    }
    #[inline]
    fn candidate(c: Pso) -> (u64, Self) {
        let [s, p, o]: Triple = c.fact();
        (p, [s, o])
    }
    fn export(chunks: Vec<Vec<Self>>) -> PairChunks {
        PairChunks::Ids(chunks)
    }
}
