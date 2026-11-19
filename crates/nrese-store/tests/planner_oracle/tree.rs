//! Generated cases: a query tree two to four levels deep with at most one
//! drawing site. The site's group binds ?dk, its twin key: the variables in
//! scope where SPARQL evaluates the draw (a group's own solution, a LATERAL's
//! left row, the row an EXISTS tests). The scope query evaluates the pattern
//! whose solutions are those evaluations, so it counts them per key.

use crate::generator::{self, Case, PLACES, REMOTE, Rng};
use crate::routes::ENDPOINT;
use crate::twin::{Draw, Query, key_expression};

// A pattern and the variables it may bind.
type Node = (String, Vec<&'static str>);

const DK: &[&str] = &["dk"];

fn merge(mut vars: Vec<&'static str>, more: &[&'static str]) -> Vec<&'static str> {
    for var in more {
        if !vars.contains(var) {
            vars.push(var);
        }
    }
    vars
}

fn key(vars: &[&'static str]) -> String {
    format!("BIND({} AS ?dk)", key_expression("k", vars))
}

struct Tree {
    rng: Rng,
    subjects: usize,
    predicates: usize,
    labels: Vec<&'static str>,
}

impl Tree {
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.rng.below(items.len())]
    }

    fn label(&mut self, label: &'static str) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn subject(&mut self) -> String {
        format!("<urn:s:{}>", self.rng.below(self.subjects))
    }

    // A ring over <urn:p:0>, integers on the other predicates, a few
    // <urn:few> statements and one wide subject; all of it again in <urn:g>.
    fn data(&mut self) -> Vec<String> {
        let mut data = Vec::new();
        for s in 0..self.subjects {
            let next = (s + 1) % self.subjects;
            data.push(format!("<urn:s:{s}> <urn:p:0> <urn:s:{next}> ."));
            for p in 1..self.predicates {
                if p == 1 || self.rng.below(2) == 0 {
                    data.push(format!("<urn:s:{s}> <urn:p:{p}> {} .", self.rng.below(8)));
                }
            }
            if s % 3 == 0 {
                data.push(format!("<urn:s:{s}> <urn:few> {s} ."));
            }
        }
        data.extend((0..100).map(|o| format!("<urn:wide> <urn:p:1> {o} .")));
        let named: Vec<_> = data.iter().map(|t| format!("<urn:g> {{ {t} }}")).collect();
        data.extend(named);
        data
    }

    fn bgp(&mut self) -> Node {
        let p = 1 + self.rng.below(self.predicates - 1);
        match self.rng.below(3) {
            0 => (format!("?s <urn:p:{p}> ?o"), vec!["s", "o"]),
            1 => (
                format!("?s <urn:p:{p}> ?o . ?s <urn:p:0> ?t"),
                vec!["s", "o", "t"],
            ),
            _ => (
                format!("?s <urn:p:0> ?t . ?t <urn:p:{p}> ?o"),
                vec!["s", "t", "o"],
            ),
        }
    }

    fn path(&mut self) -> Node {
        let path = self.pick(&[
            "?s <urn:p:0>+ ?o",
            "<urn:s:0> <urn:p:0>* ?o",
            "?s <urn:p:0>/<urn:p:1> ?o",
            "?s ^<urn:p:0> ?o",
        ]);
        let vars = if path.starts_with('?') {
            vec!["s", "o"]
        } else {
            vec!["o"]
        };
        (path.into(), vars)
    }

    // VALUES over ?s, or ?s and ?tag, with duplicate rows and UNDEF cells.
    fn values(&mut self) -> Node {
        let pair = self.rng.below(2) == 0;
        let mut rows = Vec::new();
        for _ in 0..1 + self.rng.below(40) {
            let s = match self.rng.below(5) {
                0 => "UNDEF".to_string(),
                _ => self.subject(),
            };
            rows.push(if pair {
                format!("({s} {})", self.rng.below(5))
            } else {
                s
            });
        }
        let mut distinct = rows.clone();
        distinct.sort();
        distinct.dedup();
        if distinct.len() < rows.len() || rows.iter().any(|r| r.contains("UNDEF")) {
            self.label("values-undef");
        }
        let (head, vars) = if pair {
            ("(?s ?tag)", vec!["s", "tag"])
        } else {
            ("?s", vec!["s"])
        };
        (format!("VALUES {head} {{ {} }}", rows.join(" ")), vars)
    }

    fn leaf(&mut self) -> Node {
        match self.rng.below(4) {
            0 => self.values(),
            1 => self.path(),
            _ => self.bgp(),
        }
    }

    // One operator above `node`. The node is always a group of its own, so a
    // draw in it stays in its group. GRAPH also moves the scope query.
    fn wrap(&mut self, (text, vars): Node, scope: &mut Option<String>) -> Node {
        match self.rng.below(9) {
            0 => (
                format!("{{ {text} }} VALUES ?copy {{ 0 1 }}"),
                merge(vars, &["copy"]),
            ),
            1 | 2 => {
                let (sibling, more) = self.leaf();
                (format!("{{ {text} }} {sibling}"), merge(vars, &more))
            }
            3 => {
                self.label("union-arm");
                let (other, more) = self.leaf();
                let arms = match self.rng.below(2) {
                    0 => format!("{{ {text} }} UNION {{ {other} }}"),
                    _ => format!("{{ {other} }} UNION {{ {text} }}"),
                };
                (format!("{{ {arms} }}"), merge(vars, &more))
            }
            4 => {
                let (left, more) = self.leaf();
                (
                    format!("{left} OPTIONAL {{ {{ {text} }} }}"),
                    merge(more, &vars),
                )
            }
            5 => {
                self.label("graph");
                let graph = self.pick(&["<urn:g>", "?g"]);
                if let Some(scope) = scope {
                    *scope = format!("GRAPH {graph} {{ {scope} }}");
                }
                let vars = if graph == "?g" {
                    merge(vars, &["g"])
                } else {
                    vars
                };
                (format!("GRAPH {graph} {{ {text} }}"), vars)
            }
            6 => (format!("{{ {text} }} MINUS {{ ?s <urn:few> ?x }}"), vars),
            7 => {
                let var = self.pick(&vars);
                let test = format!("!BOUND(?{var}) || !sameTerm(?{var}, 3)");
                (format!("{{ {text} }} FILTER({test})"), vars)
            }
            _ => (format!("{{ SELECT * {{ {text} }} }}"), vars),
        }
    }

    fn tree(&mut self, depth: usize) -> Node {
        let mut node = self.leaf();
        for _ in 0..depth {
            node = self.wrap(node, &mut None);
        }
        node
    }

    // A drawing BIND or FILTER, to follow the key's BIND in the site's group.
    fn draw(&mut self, vars: Vec<&'static str>) -> (&'static str, Draw, Vec<&'static str>) {
        if self.rng.below(2) == 0 {
            let call = self.pick(&["BNODE()", "UUID()", "STRUUID()"]);
            let vars = merge(vars, &["dk", "d"]);
            ("BIND($DRAW AS ?d)", generator::value(call, DK), vars)
        } else {
            (
                "FILTER($DRAW)",
                generator::filter(DK, false),
                merge(vars, &["dk"]),
            )
        }
    }

    // The node where `place` draws, its draw, and the pattern whose solutions
    // are the draw's evaluations, each with its ?dk.
    fn site(&mut self, place: &str, case: &mut Case) -> (Node, Draw, String) {
        match place {
            "exists" => {
                let (inner, vars) = self.leaf();
                let rows = generator::keys(1 + self.rng.below(60));
                let tested = format!("VALUES ?i {{ {rows} }} {inner}");
                let vars = merge(vars, &["i"]);
                let key = key(&vars);
                let body = self.pick(&["?x <urn:few> ?y", "?s <urn:few> ?y"]);
                let text = format!("{tested} {key} FILTER EXISTS {{ {body} FILTER($DRAW) }}");
                let draw = generator::filter(DK, true);
                ((text, merge(vars, DK)), draw, format!("{tested} {key}"))
            }
            "lateral" => {
                let rows: Vec<_> = (0..1 + self.rng.below(8))
                    .map(|i| format!("({} {i})", self.subject()))
                    .collect();
                let left = format!(
                    "VALUES (?s ?i) {{ {} }} {}",
                    rows.join(" "),
                    key(&["s", "i"])
                );
                let (right, more) = self.bgp();
                let (draw_text, draw, vars) = self.draw(merge(vec!["s", "i"], &more));
                let text = format!("{{ {left} }} LATERAL {{ {{ {right} {draw_text} }} }}");
                (
                    (text, vars),
                    draw,
                    format!("{{ {left} }} LATERAL {{ {right} }}"),
                )
            }
            "service" => {
                case.remote = Some(REMOTE);
                let site = format!("<urn:a> <urn:remote> ?z {}", key(&["z"]));
                let (draw_text, draw, vars) = self.draw(vec!["k", "z"]);
                let rows = generator::keys(1 + self.rng.below(300));
                let text = format!(
                    "VALUES ?k {{ {rows} }} SERVICE <{ENDPOINT}> {{ {{ {site} {draw_text} }} UNION {{ ?k <urn:missing> ?z }} }}"
                );
                (
                    (text, vars),
                    draw,
                    format!("SERVICE <{ENDPOINT}> {{ {site} }}"),
                )
            }
            "vector" => {
                case.vector = true;
                generator::vectors(&mut case.data, 4 + self.rng.below(16));
                let (pattern, vars) = self.pick(&[
                    ("?s <urn:embedding> ?v", &["s", "v"][..]),
                    ("?x <urn:embedding> ?w", &["x", "w"][..]),
                ]);
                let key = key(vars);
                let (draw_text, draw, vars) = self.draw(merge(vars.to_vec(), &["v"]));
                let search = generator::vector_search();
                let text = format!("{{ {pattern} {key} {draw_text} }} {search}");
                ((text, vars), draw, format!("{pattern} {key}"))
            }
            // "nested" and "path": a leaf's own group.
            _ => {
                let (inner, vars) = if place == "path" {
                    self.path()
                } else {
                    self.leaf()
                };
                let key = key(&vars);
                let (draw_text, draw, vars) = self.draw(vars);
                (
                    (format!("{inner} {key} {draw_text}"), vars),
                    draw,
                    format!("{inner} {key}"),
                )
            }
        }
    }
}

pub fn generated(seed: u64) -> Case {
    let mut rng = Rng(seed);
    let place = PLACES[rng.below(PLACES.len())];
    let subjects = 2 + rng.below(39);
    let predicates = 2 + rng.below(5);
    let mut tree = Tree {
        rng,
        subjects,
        predicates,
        labels: vec![place],
    };
    let mut case = Case::new(place, tree.data(), Query::plain(String::new()));
    let depth = 1 + tree.rng.below(3);
    let mut reduced = false;
    let (text, draw, scope) = match place {
        "none" => {
            let (body, vars) = tree.tree(depth);
            let text = match tree.rng.below(4) {
                0 => {
                    reduced = true;
                    format!("SELECT REDUCED * {{ {body} }}")
                }
                1 => format!("SELECT ?s (COUNT(*) AS ?n) {{ {body} }} GROUP BY ?s"),
                // DISTINCT over all its columns, sorted by all of them: the
                // LIMIT then takes a determined part.
                2 => {
                    let order: Vec<_> = vars.iter().map(|v| format!("?{v}")).collect();
                    format!(
                        "SELECT DISTINCT * {{ {body} }} ORDER BY {} LIMIT 3",
                        order.join(" ")
                    )
                }
                _ => format!("SELECT * {{ {body} }}"),
            };
            (text, None, None)
        }
        "outer" => {
            let (body, vars) = tree.tree(depth);
            let key = key(&vars);
            let (draw_text, draw, _) = tree.draw(vars);
            let text =
                format!("SELECT * {{ {{ {body} {key} {draw_text} }} VALUES ?copy {{ 0 1 }} }}");
            (
                text,
                Some(draw),
                Some(format!("SELECT ?dk {{ {body} {key} }}")),
            )
        }
        "aggregate" => {
            let (body, _) = tree.tree(depth);
            let text = format!("SELECT ?s (AVG($DRAW) AS ?avg) {{ {body} }} GROUP BY ?s");
            (text, Some(Draw::Average), None)
        }
        _ => {
            let (mut node, draw, scope) = tree.site(place, &mut case);
            let mut scope = Some(scope);
            for _ in 0..depth {
                node = tree.wrap(node, &mut scope);
            }
            let scope = scope.map(|s| format!("SELECT ?dk {{ {s} }}"));
            (format!("SELECT * {{ {} }}", node.0), Some(draw), scope)
        }
    };
    case.labels = tree.labels;
    case.query = Query {
        text,
        draw,
        scope,
        reduced,
    };
    case
}
