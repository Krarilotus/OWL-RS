//! Hyperresolution with compiled triggers (JAIR 2009, the Hyp-rule; docs/design/owl2-dl.md
//! §6): a new fact runs only the join plans whose trigger atom it matches, and each plan
//! joins the clause's other body atoms over the node and edge indexes, from the bound
//! variables outwards. The matches are collected first and applied afterwards, so the
//! join reads a graph nobody changes under it.

use super::depset::{DepSetId, DepSets};
use super::graph::{Graph, NONE, flag};
use super::program::{Body, Head, HtClause, MAX_VARS, Plan, Program, Step};

/// The most instances one join may collect: past it the join stops, and the engine gives
/// up (a bound on memory, never a dropped instance).
pub const MAX_FIRINGS: usize = 1 << 20;

/// A clause instance whose head doesn't hold yet.
#[derive(Debug, Clone, Copy)]
pub struct Firing {
    pub clause: u32,
    pub bind: [u32; MAX_VARS],
    pub dep: DepSetId,
}

/// The clause instances the concept fact `fact` completes; `tried` counts the plans run.
pub fn join_concept(
    p: &Program,
    g: &Graph,
    deps: &mut DepSets,
    fact: u32,
    out: &mut Vec<Firing>,
    tried: &mut u64,
) {
    let f = g.unary[fact as usize];
    let Some(plans) = p.by_concept.get(f.concept as usize) else {
        return;
    };
    *tried += plans.len() as u64;
    for plan in plans {
        let clause = &p.clauses[plan.clause as usize];
        let Body::Concept(_, v) = clause.body[plan.trigger as usize] else {
            continue;
        };
        let mut bind = [NONE; MAX_VARS];
        bind[v as usize] = f.node;
        run(g, deps, plan, clause, 0, &mut bind, f.dep, out);
    }
}

/// The clause instances the edge `edge` completes; `tried` counts the plans run.
pub fn join_edge(
    p: &Program,
    g: &Graph,
    deps: &mut DepSets,
    edge: u32,
    out: &mut Vec<Firing>,
    tried: &mut u64,
) {
    let e = g.edges[edge as usize];
    let r = e.role as usize;
    let mut start = |plan: &Plan| {
        let clause = &p.clauses[plan.clause as usize];
        let Body::Role(_, a, b) = clause.body[plan.trigger as usize] else {
            return;
        };
        if a == b && e.from != e.to {
            return;
        }
        *tried += 1;
        let mut bind = [NONE; MAX_VARS];
        bind[a as usize] = e.from;
        bind[b as usize] = e.to;
        run(g, deps, plan, clause, 0, &mut bind, e.dep, out);
    };
    for plan in p.by_role.get(r).into_iter().flatten() {
        start(plan);
    }
    // The keyed plans start with a check of a concept of `from` or `to`: those the ends
    // have, through their labels, where they are fewer (the check runs either way).
    let Some(keyed) = p.keyed.get(r).filter(|k| !k.is_empty()) else {
        return;
    };
    let labels = g.labels(e.from).count() + g.labels(e.to).count();
    if keyed.len() <= labels {
        keyed.iter().for_each(&mut start);
        return;
    }
    for (node, end) in [(e.from, false), (e.to, true)] {
        if end && e.from == e.to {
            break;
        }
        for f in g.labels(node) {
            for &k in p
                .keyed_by
                .get(&(e.role, f.concept, end))
                .into_iter()
                .flatten()
            {
                start(&keyed[k as usize]);
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the join's recursion state, kept on the stack"
)]
fn run(
    g: &Graph,
    deps: &mut DepSets,
    plan: &Plan,
    clause: &HtClause,
    step: usize,
    bind: &mut [u32; MAX_VARS],
    dep: DepSetId,
    out: &mut Vec<Firing>,
) {
    if out.len() >= MAX_FIRINGS {
        return;
    }
    if step > 0
        && plan.heads_after[step - 1]
            .iter()
            .any(|&h| atom_holds(g, clause.head[h as usize], bind))
    {
        return;
    }
    let Some(&s) = plan.steps.get(step) else {
        if !head_holds(g, clause, bind) {
            out.push(Firing {
                clause: plan.clause,
                bind: *bind,
                dep,
            });
        }
        return;
    };
    match s {
        Step::Check(atom) => {
            let found = match clause.body[atom as usize] {
                Body::Concept(c, v) => g
                    .concept(bind[v as usize], c)
                    .map(|i| g.unary[i as usize].dep),
                Body::Role(r, a, b) => g
                    .edge(r, bind[a as usize], bind[b as usize])
                    .map(|i| g.edges[i as usize].dep),
            };
            if let Some(d) = found {
                let dep = deps.union(dep, d);
                run(g, deps, plan, clause, step + 1, bind, dep, out);
            }
        }
        Step::Extend {
            atom,
            from,
            to,
            forward,
        } => {
            let Body::Role(r, _, _) = clause.body[atom as usize] else {
                return;
            };
            let at = bind[from as usize];
            if forward {
                for (_, e) in g.out_edges(at) {
                    if e.role == r && g.live(e.to) {
                        bind[to as usize] = e.to;
                        let dep = deps.union(dep, e.dep);
                        run(g, deps, plan, clause, step + 1, bind, dep, out);
                    }
                }
            } else {
                for (_, e) in g.in_edges(at) {
                    if e.role == r && g.live(e.from) {
                        bind[to as usize] = e.from;
                        let dep = deps.union(dep, e.dep);
                        run(g, deps, plan, clause, step + 1, bind, dep, out);
                    }
                }
            }
            bind[to as usize] = NONE;
        }
    }
}

/// Whether a head atom already holds (a quick filter; nominals are left to the engine).
fn head_holds(g: &Graph, clause: &HtClause, bind: &[u32; MAX_VARS]) -> bool {
    clause.head.iter().any(|&h| atom_holds(g, h, bind))
}

fn atom_holds(g: &Graph, h: Head, bind: &[u32; MAX_VARS]) -> bool {
    let b = |v: u8| bind[v as usize];
    match h {
        Head::Concept(c, v) => g.concept(b(v), c).is_some(),
        Head::Role(r, x, y) => g.edge(r, b(x), b(y)).is_some(),
        Head::AtLeast(n, v) => g.number(b(v), false, n).is_some(),
        Head::AtMost(n, v) => g.number(b(v), true, n).is_some(),
        // Not where the NI rule applies (a blockable non-successor of a root x).
        Head::Equal(x, y) => {
            let (s, t) = (g.find(b(x)), g.find(b(y)));
            let (root, node) = (&g.nodes[b(0) as usize], &g.nodes[s as usize]);
            s == t
                && !(root.flags & flag::ROOT != 0
                    && node.flags & flag::ROOT == 0
                    && node.parent != b(0))
        }
        Head::Nominal(..) => false,
        // Recorded between the two values' representatives (the theory decides the rest).
        Head::Unequal(x, y) => g.unequal(g.find(b(x)), g.find(b(y))).is_some(),
    }
}
