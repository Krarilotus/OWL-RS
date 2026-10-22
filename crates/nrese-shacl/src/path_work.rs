//! Operation-scoped owned incremental path prefixes. Payload precedes its existing
//! borrowed Reservation; no Arc or escaping uncharged path clone per prefix.
use crate::graph::stopped;
use crate::model::Path;
use nrese_exec::{Budget, ChargedVec, Reservation, Ticker};
use nrese_sparql::QueryEvaluationError;
type Result<T> = std::result::Result<T, QueryEvaluationError>;

pub(crate) struct OwnedPath<'b> {
    pub(crate) path: Path,
    held: Reservation<&'b Budget>,
}
impl<'b> OwnedPath<'b> {
    fn unary(mut self, make: fn(Box<Path>) -> Path) -> Result<Self> {
        self.held.charge(size_of::<Path>())?;
        Ok(Self {
            path: make(Box::new(self.path)),
            held: self.held,
        })
    }
}
fn group<'b>(
    budget: &'b Budget,
    count: usize,
    alternative: bool,
    parts: impl IntoIterator<Item = Result<OwnedPath<'b>>>,
) -> Result<OwnedPath<'b>> {
    let mut held = Reservation::new(budget);
    let mut out = ChargedVec::with_capacity(budget, count)?;
    for part in parts {
        let OwnedPath { path, held: token } = part?;
        held.absorb(token);
        out.push(path)?;
    }
    let (parts, token) = out.into_parts();
    held.absorb(token);
    Ok(OwnedPath {
        path: if alternative {
            Path::Alternative(parts)
        } else {
            Path::Sequence(parts)
        },
        held,
    })
}
fn clone_within<'b>(path: &Path, budget: &'b Budget, ticker: &mut Ticker) -> Result<OwnedPath<'b>> {
    ticker.tick(1).map_err(stopped)?;
    match path {
        Path::Predicate(id) => Ok(OwnedPath {
            path: Path::Predicate(*id),
            held: Reservation::new(budget),
        }),
        Path::Sequence(parts) | Path::Alternative(parts) => group(
            budget,
            parts.len(),
            matches!(path, Path::Alternative(_)),
            parts.iter().map(|p| clone_within(p, budget, ticker)),
        ),
        Path::Inverse(p) => clone_within(p, budget, ticker)?.unary(Path::Inverse),
        Path::ZeroOrMore(p) => clone_within(p, budget, ticker)?.unary(Path::ZeroOrMore),
        Path::OneOrMore(p) => clone_within(p, budget, ticker)?.unary(Path::OneOrMore),
        Path::ZeroOrOne(p) => clone_within(p, budget, ticker)?.unary(Path::ZeroOrOne),
    }
}
fn inverse<'b>(path: &Path, budget: &'b Budget, ticker: &mut Ticker) -> Result<OwnedPath<'b>> {
    ticker.tick(1).map_err(stopped)?;
    match path {
        Path::Predicate(_) => clone_within(path, budget, ticker)?.unary(Path::Inverse),
        Path::Inverse(p) => normalised(p, budget, ticker),
        Path::Sequence(parts) => group(
            budget,
            parts.len(),
            false,
            parts.iter().rev().map(|p| inverse(p, budget, ticker)),
        ),
        Path::Alternative(parts) => group(
            budget,
            parts.len(),
            true,
            parts.iter().map(|p| inverse(p, budget, ticker)),
        ),
        Path::ZeroOrMore(p) => inverse(p, budget, ticker)?.unary(Path::ZeroOrMore),
        Path::OneOrMore(p) => inverse(p, budget, ticker)?.unary(Path::OneOrMore),
        Path::ZeroOrOne(p) => inverse(p, budget, ticker)?.unary(Path::ZeroOrOne),
    }
}
pub(crate) fn normalised<'b>(
    path: &Path,
    budget: &'b Budget,
    ticker: &mut Ticker,
) -> Result<OwnedPath<'b>> {
    ticker.tick(1).map_err(stopped)?;
    match path {
        Path::Predicate(_) => clone_within(path, budget, ticker),
        Path::Inverse(p) => inverse(p, budget, ticker),
        Path::Sequence(parts) | Path::Alternative(parts) => group(
            budget,
            parts.len(),
            matches!(path, Path::Alternative(_)),
            parts.iter().map(|p| normalised(p, budget, ticker)),
        ),
        Path::ZeroOrMore(p) => normalised(p, budget, ticker)?.unary(Path::ZeroOrMore),
        Path::OneOrMore(p) => normalised(p, budget, ticker)?.unary(Path::OneOrMore),
        Path::ZeroOrOne(p) => normalised(p, budget, ticker)?.unary(Path::ZeroOrOne),
    }
}
pub(crate) fn prefixes<'b>(
    path: &Path,
    budget: &'b Budget,
    ticker: &mut Ticker,
) -> Result<ChargedVec<OwnedPath<'b>, &'b Budget>> {
    ticker.tick(1).map_err(stopped)?;
    let mut out = ChargedVec::new(budget);
    match path {
        Path::Predicate(_) | Path::Inverse(_) => out.push(clone_within(path, budget, ticker)?)?,
        Path::Sequence(parts) => {
            for (k, part) in parts.iter().enumerate() {
                let (tails, _slots) = prefixes(part, budget, ticker)?.into_parts();
                for tail in tails {
                    let prefix = if k == 0 {
                        tail
                    } else {
                        group(
                            budget,
                            k + 1,
                            false,
                            parts[..k]
                                .iter()
                                .map(|p| clone_within(p, budget, ticker))
                                .chain(std::iter::once(Ok(tail))),
                        )?
                    };
                    out.push(prefix)?;
                }
            }
        }
        Path::Alternative(parts) => {
            for part in parts {
                let (paths, _slots) = prefixes(part, budget, ticker)?.into_parts();
                for path in paths {
                    out.push(path)?;
                }
            }
        }
        Path::ZeroOrMore(inner) | Path::OneOrMore(inner) => {
            let (tails, _slots) = prefixes(inner, budget, ticker)?.into_parts();
            for tail in tails {
                let repeated = clone_within(inner, budget, ticker)?.unary(Path::ZeroOrMore)?;
                out.push(group(budget, 2, false, [Ok(repeated), Ok(tail)])?)?;
            }
        }
        Path::ZeroOrOne(inner) => return prefixes(inner, budget, ticker),
    }
    Ok(out)
}
