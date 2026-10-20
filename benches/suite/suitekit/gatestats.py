"""The release gate's inference (benches/PROTOCOL.md §5; research round 8, 19 Oct 2026).

One question per hypothesis, declared before any result (the family): does the candidate lose
on this dimension (a case's series, metric, against one baseline)? The unit is a block: each
case runs base, main, cand, cand, main, base back to back, so every side's runs are centred on
the same instant whatever their durations (a palindrome of whole passes isn't: passes of
different lengths shift a case's time on each side). A block's contrast is the log ratio of the
candidate's median to the baseline's in that block. With builds in several code layouts each
block runs one layout on every side, and the blocks of a layout are averaged first: the layout is
the outer unit, so its variance (tau^2 / K) stays in the error however many blocks repeat it.

- The test is one-sided for a loss, a t-test on the units' mean log contrast (the model of the
  power table: independent, roughly normal log contrasts). Its calibration is checked on A/A data
  (benches/probes/gate-power.py); an exact sign-flip p-value is reported beside it where the
  units are few enough to enumerate, as a check on the t-test's tails, not used to decide.
- Rounds are prespecified: a loss counts when two consecutive rounds show it, round 1 and 2, or 2
  and 3 for one first seen while confirming another; their p-value is the larger of the two, the
  two pairs weighted 0.9 and 0.1 (a Bonferroni split, so either path stays valid).
- Holm over the whole family at alpha 0.05 decides: what it rejects is a confirmed loss, what it
  doesn't passes at the end of the budget. No interim look, no relative tolerance.
- A difference within the counter's own resolution (whole MiB, whole microseconds) that the test
  doesn't reject is `unresolved`, reported, never `neutral`.
"""
from __future__ import annotations

import itertools
import math
import statistics

ALPHA = 0.05
PAIR_WEIGHTS = (0.9, 0.1)  # rounds (1, 2), then (2, 3)
EXACT_SIGN_FLIP = 16       # units up to which the sign-flip p-value is enumerated exactly


# --- Student's t, without SciPy ----------------------------------------------------------------

def _betacf(a: float, b: float, x: float) -> float:
    """The continued fraction of the regularized incomplete beta function (Numerical Recipes)."""
    qab, qap, qam = a + b, a + 1, a - 1
    c, d = 1.0, 1.0 - qab * x / qap
    d = 1.0 / (d if abs(d) > 1e-300 else 1e-300)
    h = d
    for m in range(1, 400):
        m2 = 2 * m
        aa = m * (b - m) * x / ((qam + m2) * (a + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > 1e-300 else 1e-300)
        c = 1.0 + aa / c if abs(1.0 + aa / c) > 1e-300 else 1e-300
        h *= d * c
        aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > 1e-300 else 1e-300)
        c = 1.0 + aa / c if abs(1.0 + aa / c) > 1e-300 else 1e-300
        delta = d * c
        h *= delta
        if abs(delta - 1.0) < 1e-15:
            break
    return h


def betainc(a: float, b: float, x: float) -> float:
    """I_x(a, b), the regularized incomplete beta function."""
    if x <= 0:
        return 0.0
    if x >= 1:
        return 1.0
    front = math.exp(math.lgamma(a + b) - math.lgamma(a) - math.lgamma(b) + a * math.log(x) + b * math.log1p(-x))
    if x < (a + 1) / (a + b + 2):
        return front * _betacf(a, b, x) / a
    return 1.0 - front * _betacf(b, a, 1 - x) / b


def t_sf(t: float, df: float) -> float:
    """P(T > t) for Student's t with `df` degrees of freedom."""
    if math.isinf(t):
        return 0.0 if t > 0 else 1.0
    tail = 0.5 * betainc(df / 2, 0.5, df / (df + t * t))
    return tail if t > 0 else 1.0 - tail


def t_ppf(q: float, df: float) -> float:
    """The `q` quantile of Student's t (bisection on t_sf; for intervals, not decisions)."""
    lo, hi = -1e3, 1e3
    for _ in range(200):
        mid = (lo + hi) / 2
        if 1.0 - t_sf(mid, df) < q:
            lo = mid
        else:
            hi = mid
    return (lo + hi) / 2


# --- one hypothesis ----------------------------------------------------------------------------

def units(contrasts: list[float], layouts: list) -> list[float]:
    """The outer units: the mean contrast of each layout's blocks (each block its own unit when
    every block has its own layout, or none is recorded). With one layout the blocks are the
    units: one build per side has no layout variance to show, its layout is part of the binaries
    as built (the gate with GATE_LAYOUTS = 1)."""
    if not layouts or len(set(layouts)) in (1, len(layouts)):
        return list(contrasts)
    by: dict = {}
    for c, layout in zip(contrasts, layouts):
        by.setdefault(layout, []).append(c)
    return [statistics.fmean(v) for _, v in sorted(by.items(), key=lambda kv: str(kv[0]))]


def sign_flip_p(values: list[float]) -> float | None:
    """The exact one-sided sign-flip p-value of a mean above zero (None beyond EXACT_SIGN_FLIP)."""
    n = len(values)
    if n == 0 or n > EXACT_SIGN_FLIP:
        return None
    observed = sum(values)
    hits = sum(1 for signs in itertools.product((1, -1), repeat=n)
               if sum(s * v for s, v in zip(signs, values)) >= observed - 1e-12)
    return hits / 2 ** n


def loss_test(contrasts: list[float], layouts: list | None = None, higher_is_better: bool = False,
              resolution: float = 0.0, scale: float = 1.0, floor: float = 0.0) -> dict:
    """The one-sided test of a loss over block contrasts (log new/base, one per block), the units
    being layouts where several blocks share one. `resolution`: the counter's step, in the
    measurement's unit, and `scale` the base level it is relative to (a log difference below
    resolution / scale that the test doesn't reject is `unresolved`). `floor`: the sd of one
    block's contrast between two copies of one build on this host (its A-A' run, noise_floor());
    the units' spread is taken as at least that (a unit of k blocks: floor / sqrt(k)), so a loss is
    clear against the noise measured between instances and sessions, not only against the few
    units' own spread (lks, 20 Oct 2026: two rounds of the same two files, x1.032 and x0.951, both
    at p = 0.001)."""
    signed = [(-c if higher_is_better else c) for c in contrasts]  # positive: worse
    u = units(signed, list(layouts or []))
    out = {"blocks": len(contrasts), "units": len(u), "mean_log": None, "p": 1.0, "p_sign_flip": None,
           "ci": None, "verdict": "incomplete"}
    if len(u) < 2:
        return out
    mean = statistics.fmean(u)
    sd = statistics.stdev(u)
    if floor > 0:
        least = floor / math.sqrt(len(contrasts) / len(u))
        out["floor"] = least
        if sd < least:
            sd, out["floored"] = least, True
    out["mean_log"] = mean
    out["p_sign_flip"] = sign_flip_p(u)
    if sd == 0:
        # Every unit alike (counters in whole steps tie often): no t, the exact sign-flip p of n
        # equal units, 2^-n, never 0 (A/A, 19 Oct 2026: ties gave p = 0 in both rounds).
        out["p"] = 0.5 ** len(u) if mean > 0 else 1.0
        out["ci"] = [mean, mean]
    else:
        t = mean / (sd / math.sqrt(len(u)))
        out["p"] = t_sf(t, len(u) - 1)
        half = t_ppf(0.975, len(u) - 1) * sd / math.sqrt(len(u))
        out["ci"] = [mean - half, mean + half]
    out["verdict"] = "measured"
    if resolution and scale > 0 and abs(mean) < math.log1p(resolution / scale):
        out["below_resolution"] = True
    return out


# --- rounds and the family ---------------------------------------------------------------------

def combine_rounds(ps: list[float | None]) -> float:
    """A hypothesis's p-value over the prespecified rounds (p of rounds 1, 2, 3; None or missing:
    not run, 1): min(max(p1, p2) / 0.9, max(p2, p3) / 0.1), capped at 1."""
    p = [1.0 if x is None else x for x in (list(ps) + [None, None, None])[:3]]
    first = max(p[0], p[1]) / PAIR_WEIGHTS[0]
    second = max(p[1], p[2]) / PAIR_WEIGHTS[1]
    return min(1.0, first, second)


def holm(pvalues: dict, alpha: float = ALPHA) -> set:
    """The hypotheses Holm's step-down procedure rejects (family-wise error at most `alpha`,
    whatever the dependence)."""
    ordered = sorted(pvalues.items(), key=lambda kv: kv[1])
    m = len(ordered)
    rejected = set()
    for i, (key, p) in enumerate(ordered):
        if p > alpha / (m - i):
            break
        rejected.add(key)
    return rejected


def rejectable(p: float, m: int, alpha: float = ALPHA) -> bool:
    """Whether a hypothesis with this p-value could still be rejected by Holm in a family of m
    (the most lenient threshold is alpha itself, the last step)."""
    return p <= alpha


# --- power -------------------------------------------------------------------------------------

def power_at(n: int, sigma: float, m: int, loss: float = 0.02, alpha: float = ALPHA) -> float:
    """One round's power against a `loss` with n units of log-contrast sd `sigma`, one-sided at
    alpha / m (Holm's first threshold: the conservative end), by the noncentral t (SciPy)."""
    from scipy import stats
    if n < 2:
        return 0.0
    if sigma <= 0:
        return 1.0
    return float(stats.nct.sf(stats.t.ppf(1 - alpha / m, n - 1), n - 1, math.log1p(loss) * math.sqrt(n) / sigma))


def blocks_needed(sigma: float, m: int, loss: float = 0.02, power: float = 0.90 ** 0.5, alpha: float = ALPHA,
                  limit: int = 1000) -> int | None:
    """Blocks (independent units) a round needs for `power` against a `loss` at a log-contrast
    sd `sigma`, one-sided at alpha / m (Holm's first threshold), by the noncentral t (SciPy)."""
    from scipy import stats
    delta = math.log1p(loss)
    a = alpha / m
    for n in range(2, limit + 1):
        if stats.nct.sf(stats.t.ppf(1 - a, n - 1), n - 1, delta * math.sqrt(n) / sigma) >= power:
            return n
    return None
