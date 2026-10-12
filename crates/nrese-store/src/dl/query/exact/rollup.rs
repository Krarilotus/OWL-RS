use super::*;

/// An edge of the existential part: from a variable along a property (or its inverse) to
/// a variable or a named term.
#[derive(Debug, Clone)]
struct Edge {
    from: String,
    property: ObjProp,
    to: Slot,
}

/// The existential atoms rolled up, one test per connected part (ExactInternalisableCQ):
/// a part attached to a named term `a` is `a : ∃p.E`, one with none is "some individual
/// is an `E`". A part that isn't a tree, a variable class or a variable predicate can't
/// be rolled up.
pub(super) fn roll_up(
    o: &mut Ontology,
    snapshot: &Snapshot,
    atoms: &[[Slot; 3]],
    ids: Ids,
    data_properties: &HashSet<u64>,
) -> Result<Vec<Test>, String> {
    let mut labels: HashMap<String, Vec<ExprId>> = HashMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut vars: Vec<String> = Vec::new();
    let add_var = |v: &String, vars: &mut Vec<String>| {
        if !vars.contains(v) {
            vars.push(v.clone());
        }
    };
    for atom in atoms {
        let [s, p, v] = atom;
        let Slot::Const(p) = p else {
            return Err("a variable predicate".to_owned());
        };
        if Some(*p) == ids.rdf_type {
            let Slot::Var(x) = s else {
                return Err("a variable class".to_owned());
            };
            let Slot::Const(c) = v else {
                return Err("a variable class".to_owned());
            };
            add_var(x, &mut vars);
            let class = match Some(*c) == ids.thing {
                true => ClassExpr::Thing,
                false => ClassExpr::Class(*c),
            };
            labels
                .entry(x.clone())
                .or_default()
                .push(ExprId(o.classes.intern(class)));
            continue;
        }
        if Some(*p) == ids.same_as {
            return Err("owl:sameAs with an existential variable".to_owned());
        }
        if data_properties.contains(p) {
            let Slot::Var(x) = s else {
                return Err("a data property with an existential value".to_owned());
            };
            add_var(x, &mut vars);
            let expr = match v {
                Slot::Const(lit) if is_literal(*lit) => {
                    note_literal(o, snapshot, *lit);
                    ClassExpr::DataHasValue(*p, *lit)
                }
                Slot::Var(y) if atoms.iter().filter(|a| a.contains(v)).count() == 1 => {
                    let _ = y;
                    let literal =
                        nrese_owl::RangeId(o.ranges.intern(nrese_owl::DataRange::Literal));
                    ClassExpr::DataSome(*p, literal)
                }
                _ => return Err("a data value shared between atoms".to_owned()),
            };
            labels
                .entry(x.clone())
                .or_default()
                .push(ExprId(o.classes.intern(expr)));
            continue;
        }
        match (s, v) {
            (Slot::Var(x), Slot::Var(y)) => {
                add_var(x, &mut vars);
                add_var(y, &mut vars);
                edges.push(Edge {
                    from: x.clone(),
                    property: ObjProp::Named(*p),
                    to: Slot::Var(y.clone()),
                });
            }
            (Slot::Var(x), Slot::Const(a)) => {
                add_var(x, &mut vars);
                edges.push(Edge {
                    from: x.clone(),
                    property: ObjProp::Named(*p),
                    to: Slot::Const(*a),
                });
            }
            (Slot::Const(a), Slot::Var(x)) => {
                add_var(x, &mut vars);
                edges.push(Edge {
                    from: x.clone(),
                    property: ObjProp::Inverse(*p),
                    to: Slot::Const(*a),
                });
            }
            (Slot::Const(_), Slot::Const(_)) => unreachable!("ground atoms are tested apart"),
        }
    }
    // Connected parts over the variable-to-variable edges.
    let mut part: HashMap<String, usize> = vars
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, v)| (v, i))
        .collect();
    loop {
        let mut changed = false;
        for e in &edges {
            if let Slot::Var(y) = &e.to {
                let (a, b) = (part[&e.from], part[y]);
                if a != b {
                    let (keep, gone) = (a.min(b), a.max(b));
                    for p in part.values_mut() {
                        if *p == gone {
                            *p = keep;
                        }
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut parts: Vec<usize> = part.values().copied().collect();
    parts.sort_unstable();
    parts.dedup();
    let mut tests = Vec::new();
    for id in parts {
        let members: Vec<&String> = vars.iter().filter(|v| part[*v] == id).collect();
        let inner = edges
            .iter()
            .filter(|e| part[&e.from] == id && matches!(e.to, Slot::Var(_)))
            .count();
        if inner + 1 != members.len() {
            return Err("existential variables that form a cycle".to_owned());
        }
        let root_leaf = edges
            .iter()
            .position(|e| part[&e.from] == id && matches!(e.to, Slot::Const(_)));
        match root_leaf {
            Some(i) => {
                let leaf = &edges[i];
                let Slot::Const(a) = leaf.to else {
                    unreachable!()
                };
                let e = expr(o, &leaf.from, None, Some(i), &labels, &edges);
                let some = ExprId(
                    o.classes
                        .intern(ClassExpr::Some(leaf.property.inverse(), e)),
                );
                tests.push(Test::Axiom(Axiom::ClassAssertion(some, a)));
            }
            None => {
                let e = expr(o, members[0], None, None, &labels, &edges);
                tests.push(Test::Nonempty(e));
            }
        }
    }
    Ok(tests)
}

/// The class expression of variable `v` reached through edge `came` (and leaving out
/// the root edge `root`): its classes, an existential per other edge.
fn expr(
    o: &mut Ontology,
    v: &str,
    came: Option<usize>,
    root: Option<usize>,
    labels: &HashMap<String, Vec<ExprId>>,
    edges: &[Edge],
) -> ExprId {
    let mut parts: Vec<ExprId> = labels.get(v).cloned().unwrap_or_default();
    for (i, e) in edges.iter().enumerate() {
        if Some(i) == came || Some(i) == root {
            continue;
        }
        if e.from == v {
            let filler = match &e.to {
                Slot::Var(w) => expr(o, w, Some(i), root, labels, edges),
                Slot::Const(b) => ExprId(o.classes.intern(ClassExpr::OneOf(vec![*b]))),
            };
            parts.push(ExprId(
                o.classes.intern(ClassExpr::Some(e.property, filler)),
            ));
        } else if matches!(&e.to, Slot::Var(w) if w == v) {
            let filler = expr(o, &e.from, Some(i), root, labels, edges);
            parts.push(ExprId(
                o.classes
                    .intern(ClassExpr::Some(e.property.inverse(), filler)),
            ));
        }
    }
    parts.sort_unstable();
    parts.dedup();
    match parts.len() {
        0 => ExprId(o.classes.intern(ClassExpr::Thing)),
        1 => parts[0],
        _ => ExprId(o.classes.intern(ClassExpr::And(parts))),
    }
}
