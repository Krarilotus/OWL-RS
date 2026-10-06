//! Data for the fast suite (`benches/fast`): `perf_lab generate KIND OUT [key=value]...`.
//!
//! Every kind is deterministic for its parameters and seed, written as N-Triples (RDF 1.2
//! where it holds triple terms), and computes here, independently of the store, the
//! answers the suite checks before a time counts. They go to standard output as one JSON
//! object (`expect`), with the statement count and the parameters used. Kinds that need
//! commits also write `OUT.commits.ru`: one SPARQL update per line, one commit each.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufWriter, Write};

use serde_json::{Map, Value, json};

const EX: &str = "http://example.org/fast/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const GEO: &str = "http://www.opengis.net/ont/geosparql#";

/// SplitMix64: small, fast, and the same everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Skewed towards small values: about a power law over `0..n`.
    fn skewed(&mut self, n: u64) -> u64 {
        ((n as f64).powf(self.unit()) as u64)
            .saturating_sub(1)
            .min(n - 1)
    }
}

/// An N-Triples writer that counts its statements.
struct Nt {
    out: BufWriter<File>,
    count: u64,
}

impl Nt {
    fn t(&mut self, s: &str, p: &str, o: &str) -> std::io::Result<()> {
        self.count += 1;
        writeln!(self.out, "{s} {p} {o} .")
    }
}

fn ex(local: impl std::fmt::Display) -> String {
    format!("<{EX}{local}>")
}

fn rdf(local: &str) -> String {
    format!("<{RDF}{local}>")
}

fn rdfs(local: &str) -> String {
    format!("<{RDFS}{local}>")
}

fn owl(local: &str) -> String {
    format!("<{OWL}{local}>")
}

fn typed(lexical: impl std::fmt::Display, datatype: &str) -> String {
    format!("\"{lexical}\"^^<{XSD}{datatype}>")
}

/// `key=value` parameters, with defaults.
struct Params(BTreeMap<String, String>);

impl Params {
    fn get(&self, key: &str, default: u64) -> u64 {
        self.0
            .get(key)
            .and_then(|v| v.replace('_', "").parse().ok())
            .unwrap_or(default)
    }
}

type Expect = Map<String, Value>;

/// `perf_lab generate KIND OUT [key=value]...`
pub fn main(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let [kind, out, rest @ ..] = args else {
        return Err("usage: perf_lab generate KIND OUT [key=value]...".into());
    };
    let params = Params(
        rest.iter()
            .filter_map(|a| a.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
            .collect(),
    );
    let mut nt = Nt {
        out: BufWriter::with_capacity(1 << 20, File::create(out)?),
        count: 0,
    };
    let mut rng = Rng(params.get("seed", 1));
    let commits = format!("{out}.commits.ru");
    let mut expect = match kind.as_str() {
        "social" => social(&params, &mut rng, &mut nt)?,
        "hierarchy" => hierarchy(&params, &mut rng, &mut nt)?,
        "clique" => clique(&params, &mut rng, &mut nt, &commits)?,
        "sameas" => sameas(&params, &mut nt, &commits)?,
        "eq-addon" => eq_addon(&params, &mut nt)?,
        "fed-chain" => fed_chain(&params, &mut nt)?,
        "sameas-giant" => sameas_giant(&params, &mut nt, &commits)?,
        "dense" => dense(&params, &mut rng, &mut nt)?,
        "chain" => chain(&params, &mut nt)?,
        "layered" => layered(&params, &mut rng, &mut nt, &commits)?,
        "entities" => entities(&params, &mut rng, &mut nt)?,
        "churn" => churn(&params, &mut nt, &commits)?,
        "batches" => batches(&params, &mut nt, &commits)?,
        "geo" => geo(&params, &mut rng, &mut nt)?,
        "text" => text(&params, &mut rng, &mut nt)?,
        "vectors" => vectors(&params, &mut rng, &mut nt)?,
        "rdf12" => rdf12(&params, &mut rng, &mut nt)?,
        "graph" => graph(&params, &mut rng, &mut nt)?,
        "twins" => twins(&params, &mut rng, &mut nt, out)?,
        "dl-transitive" => dl_transitive(&params, &mut nt)?,
        "dl-number" => dl_number(&params, &mut nt)?,
        "dl-nominals" => dl_nominals(&params, &mut nt)?,
        "dl-datatypes" => dl_datatypes(&params, &mut nt)?,
        "dl-roles" => dl_roles(&params, &mut nt)?,
        "el" => el(&params, &mut rng, &mut nt)?,
        "horn" => horn(&params, &mut rng, &mut nt)?,
        other => return Err(format!("unknown kind {other}").into()),
    };
    nt.out.flush()?;
    expect.insert("statements".into(), json!(nt.count));
    let used: Map<String, Value> = params.0.into_iter().map(|(k, v)| (k, json!(v))).collect();
    println!(
        "{}",
        json!({"kind": kind, "params": used, "expect": expect})
    );
    Ok(())
}

/// A social network for the query shapes: people who follow each other (each person
/// closes a directed triangle and a 4-cycle with its neighbours), posts with authors,
/// dates, tags and likes, a tag hierarchy. About 27 statements per person.
fn social(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let n = p.get("people", 300_000);
    let words = 5000;
    let (person, name, age, born, city, follows) = (
        ex("Person"),
        ex("name"),
        ex("age"),
        ex("born"),
        ex("city"),
        ex("follows"),
    );
    let (mut contains, mut starts, mut born_1990s, mut follow_edges) = (0u64, 0u64, 0u64, 0u64);
    let mut over_60 = 0u64;
    for i in 0..n {
        let u = ex(format!("u{i}"));
        nt.t(&u, &rdf("type"), &person)?;
        let text = format!("user {i} w{} w{}", rng.skewed(words), rng.skewed(words));
        contains += u64::from(text.contains("w42"));
        starts += u64::from(text.starts_with("user 7"));
        nt.t(&u, &name, &format!("\"{text}\"@en"))?;
        let years = 18 + rng.below(70);
        over_60 += u64::from(years > 60);
        nt.t(&u, &age, &typed(years, "integer"))?;
        let year = 1950 + rng.below(55);
        born_1990s += u64::from((1990..2000).contains(&year));
        let date = format!("{year}-{:02}-{:02}", 1 + rng.below(12), 1 + rng.below(28));
        nt.t(&u, &born, &typed(date, "date"))?;
        nt.t(&u, &city, &ex(format!("c{}", i % 1000)))?;
        // 1, 2 and n-3 close a directed triangle; 1, 1, 1, n-3 a 4-cycle. Five more at random.
        let mut targets = BTreeSet::from([(i + 1) % n, (i + 2) % n, (i + n - 3) % n]);
        while targets.len() < 8 {
            let t = rng.below(n);
            if t != i {
                targets.insert(t);
            }
        }
        for t in targets {
            nt.t(&u, &follows, &ex(format!("u{t}")))?;
            follow_edges += 1;
        }
    }
    let tags = 2000;
    for t in 0..tags {
        let tag = ex(format!("t{t}"));
        nt.t(&tag, &rdfs("label"), &format!("\"tag {t}\""))?;
        if t > 0 {
            nt.t(&tag, &ex("broader"), &ex(format!("t{}", (t - 1) / 10)))?;
        }
    }
    let posts = 2 * n;
    let mut titled = 0u64;
    let mut duplicates = 0u64;
    let mut used_tags = BTreeSet::new();
    for j in 0..posts {
        let post = ex(format!("p{j}"));
        nt.t(&post, &ex("author"), &ex(format!("u{}", rng.below(n))))?;
        let created = format!(
            "{}-{:02}-{:02}T{:02}:{:02}:00",
            2020 + rng.below(6),
            1 + rng.below(12),
            1 + rng.below(28),
            rng.below(24),
            rng.below(60)
        );
        nt.t(&post, &ex("created"), &typed(created, "dateTime"))?;
        let tag = rng.skewed(tags);
        used_tags.insert(tag);
        nt.t(&post, &ex("tag"), &ex(format!("t{tag}")))?;
        // Two likes; the same person twice is one statement once loaded.
        let likes = [rng.below(n), rng.below(n)];
        duplicates += u64::from(likes[0] == likes[1]);
        for like in likes {
            nt.t(&post, &ex("likes"), &ex(format!("u{like}")))?;
        }
        nt.t(&post, &ex("score"), &typed(rng.below(101), "integer"))?;
        if j % 3 == 0 {
            titled += 1;
            nt.t(&post, &ex("title"), &format!("\"post {j}\""))?;
        }
    }
    let mut e = Expect::new();
    e.insert("people".into(), json!(n));
    e.insert("posts".into(), json!(posts));
    e.insert("follows".into(), json!(follow_edges));
    e.insert("names_containing_w42".into(), json!(contains));
    e.insert("names_starting_user_7".into(), json!(starts));
    e.insert("born_in_the_1990s".into(), json!(born_1990s));
    e.insert("older_than_60".into(), json!(over_60));
    e.insert("titled_posts".into(), json!(titled));
    e.insert("tags_used".into(), json!(used_tags.len()));
    e.insert("distinct_statements".into(), json!(nt.count - duplicates));
    Ok(e)
}

/// A deep class hierarchy: a tree of `fan`^`depth` classes, a spine of `spine` classes
/// under its root, and instances typed at the leaves and, one in `every`, at the spine's
/// bottom (each of those has `spine` inherited types).
fn hierarchy(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let (fan, depth, spine, instances, every) = (
        p.get("fan", 4),
        p.get("depth", 7),
        p.get("spine", 1500),
        p.get("instances", 1_000_000),
        p.get("every", 1000),
    );
    let classes: u64 = (0..depth).map(|d| fan.pow(d as u32)).sum();
    let first_leaf = classes - fan.pow(depth as u32 - 1);
    let (sub, class) = (rdfs("subClassOf"), owl("Class"));
    for c in 0..classes {
        nt.t(&ex(format!("H{c}")), &rdf("type"), &class)?;
        if c > 0 {
            nt.t(
                &ex(format!("H{c}")),
                &sub,
                &ex(format!("H{}", (c - 1) / fan)),
            )?;
        }
    }
    for s in 0..spine {
        let parent = if s == 0 {
            ex("H0")
        } else {
            ex(format!("S{}", s - 1))
        };
        nt.t(&ex(format!("S{s}")), &rdf("type"), &class)?;
        nt.t(&ex(format!("S{s}")), &sub, &parent)?;
    }
    // H1's subtree: the classes whose chain of parents reaches 1.
    let under_h1 = |mut c: u64| {
        while c > 1 {
            c = (c - 1) / fan;
        }
        c == 1
    };
    let (mut on_spine, mut in_h1) = (0u64, 0u64);
    for i in 0..instances {
        let class = if i % every == 0 {
            on_spine += 1;
            format!("S{}", spine - 1)
        } else {
            let leaf = first_leaf + rng.below(classes - first_leaf);
            in_h1 += u64::from(under_h1(leaf));
            format!("H{leaf}")
        };
        nt.t(&ex(format!("x{i}")), &rdf("type"), &ex(class))?;
    }
    let mut e = Expect::new();
    e.insert("classes".into(), json!(classes + spine));
    e.insert("instances_of_root".into(), json!(instances));
    e.insert("instances_of_spine_middle".into(), json!(on_spine));
    e.insert("instances_of_h1".into(), json!(in_h1));
    Ok(e)
}

/// A symmetric and transitive property over `components` random trees of `size` nodes:
/// each component closes to a clique of size² pairs (reflexive ones included). The
/// commits delete `deletes` tree edges of component 0, splitting it.
fn clique(p: &Params, rng: &mut Rng, nt: &mut Nt, commits: &str) -> std::io::Result<Expect> {
    let (components, size, deletes) = (
        p.get("components", 40),
        p.get("size", 300),
        p.get("deletes", 20),
    );
    let sim = ex("sim");
    nt.t(&sim, &rdf("type"), &owl("SymmetricProperty"))?;
    nt.t(&sim, &rdf("type"), &owl("TransitiveProperty"))?;
    let mut parents = vec![0u64; size as usize];
    for c in 0..components {
        for j in 1..size {
            let parent = rng.below(j);
            if c == 0 {
                parents[j as usize] = parent;
            }
            nt.t(
                &ex(format!("n{}", c * size + j)),
                &sim,
                &ex(format!("n{}", c * size + parent)),
            )?;
        }
    }
    // Deleted: tree edges of component 0, chosen at random; the rest is union-found.
    let mut deleted = BTreeSet::new();
    while (deleted.len() as u64) < deletes.min(size - 1) {
        deleted.insert(1 + rng.below(size - 1));
    }
    let mut out = BufWriter::new(File::create(commits)?);
    for &j in &deleted {
        writeln!(
            out,
            "DELETE DATA {{ <{EX}n{j}> <{EX}sim> <{EX}n{}> }}",
            parents[j as usize]
        )?;
    }
    out.flush()?;
    let mut root: Vec<usize> = (0..size as usize).collect();
    fn find(root: &mut [usize], mut x: usize) -> usize {
        while root[x] != x {
            root[x] = root[root[x]];
            x = root[x];
        }
        x
    }
    for (j, &parent) in parents.iter().enumerate().skip(1) {
        if !deleted.contains(&(j as u64)) {
            let (a, b) = (find(&mut root, j), find(&mut root, parent as usize));
            root[a] = b;
        }
    }
    let mut sizes: BTreeMap<usize, u64> = BTreeMap::new();
    for j in 0..size as usize {
        *sizes.entry(find(&mut root, j)).or_default() += 1;
    }
    // A node left without edges is in no pair, not even with itself.
    let after: u64 = sizes
        .values()
        .filter(|&&s| s > 1)
        .map(|s| s * s)
        .sum::<u64>()
        + (components - 1) * size * size;
    let mut e = Expect::new();
    e.insert("pairs".into(), json!(components * size * size));
    e.insert("pairs_after_commits".into(), json!(after));
    e.insert("commits".into(), json!(deleted.len()));
    Ok(e)
}

/// `owl:sameAs` chains of `size` individuals in `groups` groups, each with a value of its
/// own: equality spreads every value to the whole group.
fn sameas(p: &Params, nt: &mut Nt, commits: &str) -> std::io::Result<Expect> {
    let (groups, size) = (p.get("groups", 20_000), p.get("size", 10));
    // `merges=M`: commits that each join group 2k with group 2k + 1 by one sameAs.
    let merges = p.get("merges", 0).min(groups / 2);
    let mut out = BufWriter::new(File::create(commits)?);
    for k in 0..merges {
        writeln!(
            out,
            "INSERT DATA {{ <{EX}i{}_0> <{OWL}sameAs> <{EX}i{}_0> }}",
            2 * k,
            2 * k + 1
        )?;
    }
    out.flush()?;
    let same = owl("sameAs");
    for g in 0..groups {
        for j in 0..size {
            let x = ex(format!("i{g}_{j}"));
            nt.t(&x, &ex("val"), &format!("\"v{g}_{j}\""))?;
            nt.t(&x, &ex("group"), &ex(format!("G{g}")))?;
            if j + 1 < size {
                nt.t(&x, &same, &ex(format!("i{g}_{}", j + 1)))?;
            }
        }
        nt.t(
            &ex(format!("i{g}_0")),
            &ex("knows"),
            &ex(format!("i{}_0", (g + 1) % groups)),
        )?;
    }
    let mut e = Expect::new();
    e.insert("values".into(), json!(groups * size * size));
    e.insert("same_as_pairs".into(), json!(groups * size * size));
    e.insert("knows".into(), json!(groups * size * size));
    e.insert(
        "values_after_merges".into(),
        json!((groups - 2 * merges) * size * size + merges * 4 * size * size),
    );
    Ok(e)
}

/// `chains` chains of `length` nodes along a transitive property.
fn chain(p: &Params, nt: &mut Nt) -> std::io::Result<Expect> {
    let (chains, length) = (p.get("chains", 2), p.get("length", 2500));
    let anc = ex("before");
    nt.t(&anc, &rdf("type"), &owl("TransitiveProperty"))?;
    for c in 0..chains {
        for i in 0..length - 1 {
            nt.t(
                &ex(format!("k{c}_{i}")),
                &anc,
                &ex(format!("k{c}_{}", i + 1)),
            )?;
        }
    }
    let mut e = Expect::new();
    e.insert("pairs".into(), json!(chains * length * (length - 1) / 2));
    Ok(e)
}

/// A source reaching `depth` layers of `width` nodes along a transitive property, every
/// node linked to `out` random nodes of the next layer: many derivations per fact, the
/// worst case for proof-based deletion (after SSPE, Hu et al. 2018). The commits delete the
/// source's links to the first layer, one per commit, then edges inside the layers.
fn layered(p: &Params, rng: &mut Rng, nt: &mut Nt, commits: &str) -> std::io::Result<Expect> {
    let (width, depth, fanout, deletes) = (
        p.get("width", 80) as usize,
        p.get("depth", 40) as usize,
        p.get("out", 3) as usize,
        p.get("deletes", 40) as usize,
    );
    let reach = ex("reach");
    nt.t(&reach, &rdf("type"), &owl("TransitiveProperty"))?;
    let node = |l: usize, i: usize| ex(format!("l{l}_{i}"));
    let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new(); // (from id, to id); source = n
    let n = width * depth;
    for i in 0..width {
        edges.insert((n, i));
    }
    for l in 0..depth - 1 {
        for i in 0..width {
            for _ in 0..fanout {
                edges.insert((
                    l * width + i,
                    (l + 1) * width + rng.below(width as u64) as usize,
                ));
            }
        }
    }
    let name = |id: usize| {
        if id == n {
            ex("source")
        } else {
            node(id / width, id % width)
        }
    };
    for &(a, b) in &edges {
        nt.t(&name(a), &reach, &name(b))?;
    }
    // The deletions: half the source's links, then random inner edges.
    let mut gone: Vec<(usize, usize)> = (0..width / 2).map(|i| (n, i)).collect();
    let inner: Vec<_> = edges.iter().filter(|e| e.0 != n).copied().collect();
    while gone.len() < deletes {
        let e = inner[rng.below(inner.len() as u64) as usize];
        if !gone.contains(&e) {
            gone.push(e);
        }
    }
    gone.truncate(deletes);
    let mut out = BufWriter::new(File::create(commits)?);
    for &(a, b) in &gone {
        let (a, b) = (name(a), name(b));
        writeln!(out, "DELETE DATA {{ {a} {reach} {b} }}")?;
    }
    out.flush()?;
    let pairs = |edges: &BTreeSet<(usize, usize)>| {
        // Reachability by bitsets, last layer first.
        let words = (n + 1).div_ceil(64);
        let mut sets = vec![vec![0u64; words]; n + 1];
        let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n + 1];
        for &(a, b) in edges {
            succ[a].push(b);
        }
        let order: Vec<usize> = (0..n).rev().chain(std::iter::once(n)).collect();
        for v in order {
            let mut set = vec![0u64; words];
            for &s in &succ[v] {
                set[s / 64] |= 1 << (s % 64);
                for (w, x) in set.iter_mut().zip(&sets[s]) {
                    *w |= x;
                }
            }
            sets[v] = set;
        }
        let total: u64 = sets
            .iter()
            .flatten()
            .map(|w| u64::from(w.count_ones()))
            .sum();
        let from_source: u64 = sets[n].iter().map(|w| u64::from(w.count_ones())).sum();
        (total, from_source)
    };
    let (total, from_source) = pairs(&edges);
    let mut kept = edges.clone();
    for e in &gone {
        kept.remove(e);
    }
    let (total_after, from_source_after) = pairs(&kept);
    let mut e = Expect::new();
    e.insert("pairs".into(), json!(total));
    e.insert("from_source".into(), json!(from_source));
    e.insert("pairs_after_commits".into(), json!(total_after));
    e.insert("from_source_after_commits".into(), json!(from_source_after));
    e.insert("commits".into(), json!(gone.len()));
    Ok(e)
}

/// The harness's entity shape (`generate`): a type, a language-tagged label, a link and a
/// date per entity.
fn entities(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let n = p.get("entities", 5_000_000);
    for i in 0..n {
        let e = ex(format!("e{i}"));
        nt.t(&e, &rdf("type"), &ex(format!("Kind{}", i % 100)))?;
        nt.t(&e, &rdfs("label"), &format!("\"entity {i}\"@en"))?;
        nt.t(&e, &ex("link"), &ex(format!("e{}", rng.below(n))))?;
        let date = format!(
            "{}-{:02}-{:02}",
            1900 + rng.below(125),
            1 + rng.below(12),
            1 + rng.below(28)
        );
        nt.t(&e, &ex("date"), &typed(date, "date"))?;
    }
    let mut e = Expect::new();
    e.insert("entities".into(), json!(n));
    Ok(e)
}

/// A small university-like schema and `people` people, and commits that each add a
/// person with types and links and later remove them: after the last commit the store
/// holds what it held before, so the closure must too.
fn churn(p: &Params, nt: &mut Nt, commits: &str) -> std::io::Result<Expect> {
    let (people, changes) = (p.get("people", 200_000), p.get("changes", 1000));
    let sub = rdfs("subClassOf");
    for (a, b) in [
        ("Student", "Person"),
        ("GraduateStudent", "Student"),
        ("Professor", "Faculty"),
        ("Faculty", "Person"),
    ] {
        nt.t(&ex(a), &sub, &ex(b))?;
    }
    nt.t(&ex("advisor"), &rdfs("domain"), &ex("Student"))?;
    nt.t(&ex("advisor"), &rdfs("range"), &ex("Professor"))?;
    nt.t(&ex("advisee"), &owl("inverseOf"), &ex("advisor"))?;
    nt.t(&ex("memberOf"), &rdfs("subPropertyOf"), &ex("worksWith"))?;
    // `violate=1`: students and professors are disjoint, and the last commit makes a
    // student a professor: the OWL 2 RL gate rejects it (cax-dw). `violate=2`: a visitor is
    // faculty or a professor (a union on the right, outside RL) and disjoint from both, and
    // the last commit adds a visitor: inconsistent only under OWL 2 DL, so only the DL gate
    // can reject it.
    let violate = p.get("violate", 0);
    if violate == 1 {
        nt.t(&ex("Student"), &owl("disjointWith"), &ex("Professor"))?;
    }
    if violate == 2 {
        let union = list(nt, "visitor", &[ex("Faculty"), ex("Professor")])?;
        nt.t("_:visitorOf", &owl("unionOf"), &union)?;
        nt.t(&ex("Visitor"), &rdfs("subClassOf"), "_:visitorOf")?;
        nt.t(&ex("Visitor"), &owl("disjointWith"), &ex("Faculty"))?;
        nt.t(&ex("Visitor"), &owl("disjointWith"), &ex("Professor"))?;
    }
    for i in 0..people {
        let x = ex(format!("s{i}"));
        nt.t(&x, &rdf("type"), &ex("GraduateStudent"))?;
        nt.t(&x, &ex("advisor"), &ex(format!("prof{}", i % 500)))?;
        nt.t(&x, &ex("memberOf"), &ex(format!("dept{}", i % 50)))?;
    }
    let mut out = BufWriter::new(File::create(commits)?);
    let statements = |i: u64| {
        format!(
            "<{EX}new{i}> a <{EX}GraduateStudent> ; <{EX}advisor> <{EX}prof{}> ; <{EX}memberOf> <{EX}dept{}>",
            i % 500,
            i % 50
        )
    };
    for i in 0..changes {
        writeln!(out, "INSERT DATA {{ {} }}", statements(i))?;
    }
    for i in 0..changes {
        writeln!(out, "DELETE DATA {{ {} }}", statements(i))?;
    }
    if violate == 1 {
        writeln!(out, "INSERT DATA {{ <{EX}s0> a <{EX}Professor> }}")?;
    }
    if violate == 2 {
        writeln!(out, "INSERT DATA {{ <{EX}guest0> a <{EX}Visitor> }}")?;
    }
    // `existing=N`: N people of the data deleted, one per commit (the deletion tail).
    for i in 0..p.get("existing", 0) {
        writeln!(
            out,
            "DELETE DATA {{ <{EX}s{i}> a <{EX}GraduateStudent> ; <{EX}advisor> <{EX}prof{}> ; <{EX}memberOf> <{EX}dept{}> }}",
            i % 500,
            i % 50
        )?;
    }
    out.flush()?;
    let mut e = Expect::new();
    e.insert("people".into(), json!(people));
    e.insert("commits".into(), json!(2 * changes + p.get("existing", 0)));
    Ok(e)
}

/// Points with WKT geometries in [0, 100)², none on a query square's edge; the expected
/// counts of four squares.
fn geo(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let n = p.get("points", 1_000_000);
    let squares = [(10.5, 20.5), (40.25, 45.25), (0.75, 50.75), (77.0, 99.0)];
    let mut inside = [0u64; 4];
    for i in 0..n {
        // Four decimals plus half a step: never on an edge at a whole or half number.
        let x = rng.below(1_000_000) as f64 / 10_000.0 + 0.00005;
        let y = rng.below(1_000_000) as f64 / 10_000.0 + 0.00005;
        for (k, (lo, hi)) in squares.iter().enumerate() {
            inside[k] += u64::from(x > *lo && x < *hi && y > *lo && y < *hi);
        }
        let point = ex(format!("pt{i}"));
        let geometry = ex(format!("g{i}"));
        nt.t(&point, &format!("<{GEO}hasGeometry>"), &geometry)?;
        nt.t(
            &geometry,
            &format!("<{GEO}asWKT>"),
            &format!("\"POINT({x:.5} {y:.5})\"^^<{GEO}wktLiteral>"),
        )?;
    }
    let mut e = Expect::new();
    for (k, count) in inside.iter().enumerate() {
        e.insert(format!("within_{k}"), json!(count));
    }
    Ok(e)
}

/// Documents of 5 to 12 words from a skewed vocabulary; how many contain given words, and
/// words with a given prefix.
fn text(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let (docs, vocabulary) = (p.get("docs", 1_000_000), p.get("vocabulary", 20_000));
    let probes = ["w3", "w77", "w1234", "w19999"];
    let mut found = [0u64; 4];
    let mut prefixed = 0u64;
    for i in 0..docs {
        let words: Vec<String> = (0..5 + rng.below(8))
            .map(|_| format!("w{}", rng.skewed(vocabulary)))
            .collect();
        for (k, probe) in probes.iter().enumerate() {
            found[k] += u64::from(words.iter().any(|w| w == probe));
        }
        prefixed += u64::from(words.iter().any(|w| w.starts_with("w12")));
        nt.t(
            &ex(format!("d{i}")),
            &rdfs("label"),
            &format!("\"{}\"", words.join(" ")),
        )?;
    }
    let mut e = Expect::new();
    for (k, probe) in probes.iter().enumerate() {
        e.insert(format!("docs_with_{probe}"), json!(found[k]));
    }
    e.insert("docs_with_prefix_w12".into(), json!(prefixed));
    Ok(e)
}

/// Embeddings of `dim` dimensions with a kind each, and the exact cosine 10 nearest of
/// kind 3 to five query vectors (`OUT` lists the query vectors in `expect`).
fn vectors(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let (n, dim, kinds) = (
        p.get("items", 200_000) as usize,
        p.get("dim", 64) as usize,
        p.get("kinds", 10) as usize,
    );
    let mut data: Vec<Vec<f32>> = Vec::with_capacity(n);
    for i in 0..n {
        let v: Vec<f32> = (0..dim).map(|_| rng.unit() as f32 * 2.0 - 1.0).collect();
        let item = ex(format!("item{i}"));
        nt.t(
            &item,
            &ex("embedding"),
            &format!(
                "\"{}\"^^<{}>",
                nrese_engine::vector::lexical(&v),
                nrese_engine::vector::DATATYPE
            ),
        )?;
        nt.t(&item, &ex("kind"), &ex(format!("k{}", i % kinds)))?;
        data.push(v);
    }
    let cosine = |a: &[f32], b: &[f32]| {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (na * nb)
    };
    let mut e = Expect::new();
    for q in 0..5 {
        let query: Vec<f32> = (0..dim).map(|_| rng.unit() as f32 * 2.0 - 1.0).collect();
        let mut scored: Vec<(f32, usize)> = (0..n)
            .filter(|i| i % kinds == 3)
            .map(|i| (cosine(&query, &data[i]), i))
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        e.insert(
            format!("query_{q}"),
            json!(nrese_engine::vector::lexical(&query)),
        );
        e.insert(
            format!("nearest_{q}"),
            json!(
                scored[..10]
                    .iter()
                    .map(|(_, i)| format!("item{i}"))
                    .collect::<Vec<_>>()
            ),
        );
    }
    Ok(e)
}

/// Statements with reifiers (RDF 1.2 triple terms) that carry a source and a confidence.
fn rdf12(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let (n, sources) = (p.get("statements", 1_000_000), p.get("sources", 100));
    let mut by_source = vec![0u64; sources as usize];
    let mut confident = 0u64;
    for i in 0..n {
        let (s, o) = (
            ex(format!("a{}", i / 4)),
            ex(format!("b{}", rng.below(n / 4))),
        );
        let rel = ex("rel");
        nt.t(&s, &rel, &o)?;
        let r = ex(format!("r{i}"));
        nt.t(&r, &rdf("reifies"), &format!("<<( {s} {rel} {o} )>>"))?;
        let source = (i % sources) as usize;
        by_source[source] += 1;
        nt.t(&r, &ex("source"), &ex(format!("src{source}")))?;
        let confidence = rng.below(100);
        confident += u64::from(confidence >= 90);
        nt.t(
            &r,
            &ex("confidence"),
            &typed(format!("0.{confidence:02}"), "decimal"),
        )?;
    }
    let mut e = Expect::new();
    e.insert("reifiers".into(), json!(n));
    e.insert("from_source_7".into(), json!(by_source[7]));
    e.insert("confident".into(), json!(confident));
    Ok(e)
}

/// A random directed graph (`edge`), for transitive closure as user rules (N3) and as a
/// Datalog program for Nemo; the closure's size computed by a search from every node.
fn graph(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let (n, m) = (p.get("nodes", 2000) as usize, p.get("edges", 3000) as usize);
    let mut edges = BTreeSet::new();
    while edges.len() < m {
        let (a, b) = (rng.below(n as u64) as usize, rng.below(n as u64) as usize);
        edges.insert((a, b));
    }
    let mut succ = vec![Vec::new(); n];
    for &(a, b) in &edges {
        succ[a].push(b);
        nt.t(&ex(format!("v{a}")), &ex("edge"), &ex(format!("v{b}")))?;
    }
    let mut pairs = 0u64;
    let mut seen = vec![usize::MAX; n];
    for start in 0..n {
        let mut stack = succ[start].clone();
        while let Some(v) = stack.pop() {
            if seen[v] != start {
                seen[v] = start;
                pairs += 1;
                stack.extend(&succ[v]);
            }
        }
    }
    let mut e = Expect::new();
    e.insert("edges".into(), json!(edges.len()));
    e.insert("path_pairs".into(), json!(pairs));
    Ok(e)
}

/// Blank nodes that canonicalisation must not branch on: one node with `twins` equal
/// children, and `rings` rings of `ring` nodes (automorphisms that aren't twins). Also
/// writes `OUT.shuffled.nt`, the same graph with other labels in another order: both
/// canonicalise to the same quads.
fn twins(p: &Params, rng: &mut Rng, nt: &mut Nt, out: &str) -> std::io::Result<Expect> {
    let (twins, rings, ring) = (p.get("twins", 2000), p.get("rings", 50), p.get("ring", 12));
    let mut lines = Vec::new();
    for i in 0..twins {
        lines.push(format!("_:root {} _:c{i}", ex("child")));
        lines.push(format!("_:c{i} {} \"same\"", ex("v")));
    }
    for r in 0..rings {
        for i in 0..ring {
            lines.push(format!(
                "_:r{r}_{i} {} _:r{r}_{}",
                ex("next"),
                (i + 1) % ring
            ));
        }
        lines.push(format!("_:r{r}_0 {} \"ring\"", ex("v")));
    }
    for line in &lines {
        nt.count += 1;
        writeln!(nt.out, "{line} .")?;
    }
    // Relabelled (a suffix on every label) and shuffled.
    let mut shuffled: Vec<String> = lines.iter().map(|l| l.replace("_:", "_:x")).collect();
    for i in (1..shuffled.len()).rev() {
        shuffled.swap(i, rng.below(i as u64 + 1) as usize);
    }
    let mut other = BufWriter::new(File::create(format!("{out}.shuffled.nt"))?);
    for line in &shuffled {
        writeln!(other, "{line} .")?;
    }
    other.flush()?;
    Ok(Expect::new())
}

/// Writes `node a owl:Restriction ; owl:onProperty property` and returns the node.
fn restriction(nt: &mut Nt, id: &str, property: &str) -> std::io::Result<String> {
    let node = format!("_:{id}");
    nt.t(&node, &rdf("type"), &owl("Restriction"))?;
    nt.t(&node, &owl("onProperty"), property)?;
    Ok(node)
}

/// An RDF list of `items`; returns its head.
fn list(nt: &mut Nt, id: &str, items: &[String]) -> std::io::Result<String> {
    let mut next = rdf("nil");
    for (i, item) in items.iter().enumerate().rev() {
        let cell = format!("_:{id}_{i}");
        nt.t(&cell, &rdf("first"), item)?;
        nt.t(&cell, &rdf("rest"), &next)?;
        next = cell;
    }
    Ok(next)
}

/// Definitions `C_i ≡ ∃hasPart.D_i` over a transitive `hasPart`, with `D_i ⊑
/// ∃hasPart.D_(i+1)` around a cycle and an instance of each `D_i` (the shape of
/// ore_ont_10212). Consistent; Horn once `∃R.D ⊑ C` over a transitive R is read as
/// `D ⊑ ∀R⁻.C`, so the tableau never branches.
fn dl_transitive(p: &Params, nt: &mut Nt) -> std::io::Result<Expect> {
    let n = p.get("definitions", 400);
    let has_part = ex("hasPart");
    nt.t(&has_part, &rdf("type"), &owl("ObjectProperty"))?;
    nt.t(&has_part, &rdf("type"), &owl("TransitiveProperty"))?;
    for i in 0..n {
        let (c, d) = (ex(format!("C{i}")), ex(format!("D{i}")));
        let r = restriction(nt, &format!("def{i}"), &has_part)?;
        nt.t(&r, &owl("someValuesFrom"), &d)?;
        nt.t(&c, &owl("equivalentClass"), &r)?;
        let s = restriction(nt, &format!("next{i}"), &has_part)?;
        nt.t(&s, &owl("someValuesFrom"), &ex(format!("D{}", (i + 1) % n)))?;
        nt.t(&d, &rdfs("subClassOf"), &s)?;
        nt.t(&ex(format!("a{i}")), &rdf("type"), &d)?;
    }
    let mut e = Expect::new();
    e.insert("answer".into(), json!("consistent"));
    Ok(e)
}

/// Qualified number restrictions that must be met exactly: every `A` has at least `n`
/// R-successors in `B = B1 ⊔ B2` (disjoint), at most n/2 in each; `individuals` instances
/// of A. Consistent for even n; `over=1` asks for n+1 and is inconsistent.
fn dl_number(p: &Params, nt: &mut Nt) -> std::io::Result<Expect> {
    let (n, individuals, over) = (p.get("n", 12), p.get("individuals", 20), p.get("over", 0));
    let r = ex("R");
    nt.t(&r, &rdf("type"), &owl("ObjectProperty"))?;
    let (a, b, b1, b2) = (ex("A"), ex("B"), ex("B1"), ex("B2"));
    let nni = |k: u64| typed(k, "nonNegativeInteger");
    let at_least = restriction(nt, "atleast", &r)?;
    nt.t(&at_least, &owl("minQualifiedCardinality"), &nni(n + over))?;
    nt.t(&at_least, &owl("onClass"), &b)?;
    nt.t(&a, &rdfs("subClassOf"), &at_least)?;
    let union = list(nt, "union", &[b1.clone(), b2.clone()])?;
    nt.t(&b, &owl("unionOf"), &union)?;
    nt.t(&b1, &owl("disjointWith"), &b2)?;
    for (id, class) in [("most1", &b1), ("most2", &b2)] {
        let at_most = restriction(nt, id, &r)?;
        nt.t(&at_most, &owl("maxQualifiedCardinality"), &nni(n / 2))?;
        nt.t(&at_most, &owl("onClass"), class)?;
        nt.t(&a, &rdfs("subClassOf"), &at_most)?;
    }
    for i in 0..individuals {
        nt.t(&ex(format!("x{i}")), &rdf("type"), &a)?;
    }
    let mut e = Expect::new();
    e.insert(
        "answer".into(),
        json!(if over > 0 || n % 2 == 1 {
            "inconsistent"
        } else {
            "consistent"
        }),
    );
    Ok(e)
}

/// Nominals: a class of `colours` named colours, all different; `nodes` individuals in a
/// ring, each with exactly one colour from the class and a colour of its own differing
/// from its successor's. Consistent with at least two colours (an even ring).
fn dl_nominals(p: &Params, nt: &mut Nt) -> std::io::Result<Expect> {
    let (colours, nodes) = (p.get("colours", 3), p.get("nodes", 60));
    let has = ex("hasColour");
    nt.t(&has, &rdf("type"), &owl("ObjectProperty"))?;
    nt.t(&has, &rdf("type"), &owl("FunctionalProperty"))?;
    let names: Vec<String> = (0..colours).map(|c| ex(format!("colour{c}"))).collect();
    let one_of = list(nt, "colours", &names)?;
    let colour = ex("Colour");
    nt.t(&colour, &owl("oneOf"), &one_of)?;
    let members = list(nt, "different", &names)?;
    nt.t("_:alldiff", &rdf("type"), &owl("AllDifferent"))?;
    nt.t("_:alldiff", &owl("distinctMembers"), &members)?;
    let node = ex("Node");
    let some = restriction(nt, "some", &has)?;
    nt.t(&some, &owl("someValuesFrom"), &colour)?;
    nt.t(&node, &rdfs("subClassOf"), &some)?;
    // A colour per node, differing from the next node's: as `differentFrom` on fresh
    // colour individuals equated to the class by the functional property.
    for i in 0..nodes {
        let x = ex(format!("n{i}"));
        nt.t(&x, &rdf("type"), &node)?;
        nt.t(&x, &has, &ex(format!("c{i}")))?;
    }
    for i in 0..nodes {
        nt.t(
            &ex(format!("c{i}")),
            &owl("differentFrom"),
            &ex(format!("c{}", (i + 1) % nodes)),
        )?;
    }
    let mut e = Expect::new();
    let colourable = colours >= 3 || (colours == 2 && nodes % 2 == 0);
    e.insert(
        "answer".into(),
        json!(if colourable {
            "consistent"
        } else {
            "inconsistent"
        }),
    );
    Ok(e)
}

/// At least `n` values of `xsd:byte` (W3C I5.8-002's shape): consistent up to 256, and
/// inconsistent above.
fn dl_datatypes(p: &Params, nt: &mut Nt) -> std::io::Result<Expect> {
    let n = p.get("n", 256);
    let dp = ex("dp");
    nt.t(&dp, &rdf("type"), &owl("DatatypeProperty"))?;
    let r = restriction(nt, "bytes", &dp)?;
    nt.t(
        &r,
        &owl("minQualifiedCardinality"),
        &typed(n, "nonNegativeInteger"),
    )?;
    nt.t(&r, &owl("onDataRange"), &format!("<{XSD}byte>"))?;
    nt.t(&ex("x"), &rdf("type"), &r)?;
    let mut e = Expect::new();
    e.insert(
        "answer".into(),
        json!(if n <= 256 {
            "consistent"
        } else {
            "inconsistent"
        }),
    );
    Ok(e)
}

/// A large role hierarchy of transitive roles under `partOf`, and `DisjointClasses(A_i,
/// ∃partOf.B_i)` axioms: the universal each one needs walks the automaton of `partOf`,
/// which must stay a few states (ore_ont_1066's shape).
fn dl_roles(p: &Params, nt: &mut Nt) -> std::io::Result<Expect> {
    let (roles, axioms) = (p.get("roles", 60), p.get("axioms", 200));
    let part_of = ex("partOf");
    nt.t(&part_of, &rdf("type"), &owl("TransitiveProperty"))?;
    for r in 1..roles {
        let role = ex(format!("part{r}"));
        nt.t(&role, &rdf("type"), &owl("ObjectProperty"))?;
        if r % 2 == 0 {
            nt.t(&role, &rdf("type"), &owl("TransitiveProperty"))?;
        }
        let parent = if r < 2 {
            part_of.clone()
        } else {
            ex(format!("part{}", r / 2))
        };
        nt.t(&role, &rdfs("subPropertyOf"), &parent)?;
    }
    for i in 0..axioms {
        let some = restriction(nt, &format!("p{i}"), &part_of)?;
        nt.t(&some, &owl("someValuesFrom"), &ex(format!("B{i}")))?;
        nt.t(&ex(format!("A{i}")), &owl("disjointWith"), &some)?;
        let role = ex(format!("part{}", 1 + i % (roles - 1)));
        nt.t(&ex(format!("a{i}")), &role, &ex(format!("b{i}")))?;
        nt.t(
            &ex(format!("b{i}")),
            &rdf("type"),
            &ex(format!("B{}", (i + 1) % axioms)),
        )?;
    }
    let mut e = Expect::new();
    e.insert("answer".into(), json!("consistent"));
    Ok(e)
}

/// A random OWL 2 EL ontology of `classes` classes: a subclass DAG, existentials,
/// definitions by conjunctions, a role hierarchy with a transitive role and a chain.
/// Every class and property is declared: OWL API readers (ELK, Konclude, HermiT through the
/// DL kit) drop axioms about undeclared terms, and so classified a weaker ontology.
fn el(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let (n, roles) = (p.get("classes", 100_000), p.get("roles", 12));
    for r in 0..roles {
        nt.t(&ex(format!("r{r}")), &rdf("type"), &owl("ObjectProperty"))?;
    }
    for r in 1..roles {
        nt.t(
            &ex(format!("r{r}")),
            &rdfs("subPropertyOf"),
            &ex(format!("r{}", r / 2)),
        )?;
    }
    // A transitive role low in the hierarchy (a transitive top role chains every existential
    // to every other: a quadratic closure, not EL's typical shape).
    nt.t(
        &ex(format!("r{}", roles - 1)),
        &rdf("type"),
        &owl("TransitiveProperty"),
    )?;
    // The chain is regular by default (r1 ∘ r2 ⊑ r1, with r2 ⊑ r1), so HermiT and Openllet
    // take the ontology too. `irregular=1` puts it under r3 instead: r3 ⊑ r1 makes the role
    // hierarchy irregular, outside OWL 2 DL (OWL 2 EL allows it; HermiT refuses it).
    let chain = list(nt, "chain", &[ex("r1"), ex("r2")])?;
    let implied = if p.get("irregular", 0) == 1 {
        "r3"
    } else {
        "r1"
    };
    nt.t(&ex(implied), &owl("propertyChainAxiom"), &chain)?;
    for i in 0..n {
        let a = ex(format!("A{i}"));
        nt.t(&a, &rdf("type"), &owl("Class"))?;
        if i == 0 {
            continue;
        }
        nt.t(&a, &rdfs("subClassOf"), &ex(format!("A{}", rng.below(i))))?;
        let roll = rng.below(100);
        if roll < 30 {
            let some = restriction(nt, &format!("e{i}"), &ex(format!("r{}", rng.below(roles))))?;
            nt.t(
                &some,
                &owl("someValuesFrom"),
                &ex(format!("A{}", rng.below(n))),
            )?;
            nt.t(&a, &rdfs("subClassOf"), &some)?;
        } else if roll < 40 && i > 2 {
            let some = restriction(nt, &format!("d{i}"), &ex(format!("r{}", rng.below(roles))))?;
            nt.t(
                &some,
                &owl("someValuesFrom"),
                &ex(format!("A{}", rng.below(i))),
            )?;
            let parts = list(
                nt,
                &format!("and{i}"),
                &[ex(format!("A{}", rng.below(i))), some],
            )?;
            let and = format!("_:i{i}");
            nt.t(&and, &owl("intersectionOf"), &parts)?;
            let d = ex(format!("D{i}"));
            nt.t(&d, &rdf("type"), &owl("Class"))?;
            nt.t(&d, &owl("equivalentClass"), &and)?;
        }
    }
    Ok(Expect::new())
}

/// A Horn ontology beyond EL: inverse roles and universals over them, for the context core
/// and the hypertableau.
fn horn(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let n = p.get("classes", 20_000);
    let r = ex("r");
    // Declared, as in `el`.
    for property in [&r, &ex("rInv")] {
        nt.t(property, &rdf("type"), &owl("ObjectProperty"))?;
    }
    nt.t(&ex("rInv"), &owl("inverseOf"), &r)?;
    for i in 0..n {
        let a = ex(format!("A{i}"));
        nt.t(&a, &rdf("type"), &owl("Class"))?;
        if i == 0 {
            continue;
        }
        nt.t(&a, &rdfs("subClassOf"), &ex(format!("A{}", rng.below(i))))?;
        match rng.below(10) {
            0..=2 => {
                let some = restriction(nt, &format!("s{i}"), &r)?;
                nt.t(
                    &some,
                    &owl("someValuesFrom"),
                    &ex(format!("A{}", rng.below(n))),
                )?;
                nt.t(&a, &rdfs("subClassOf"), &some)?;
            }
            3..=4 => {
                let all = restriction(nt, &format!("u{i}"), &ex("rInv"))?;
                nt.t(
                    &all,
                    &owl("allValuesFrom"),
                    &ex(format!("A{}", rng.below(n))),
                )?;
                nt.t(&a, &rdfs("subClassOf"), &all)?;
            }
            _ => {}
        }
    }
    Ok(Expect::new())
}

/// One giant `owl:sameAs` class of `size` individuals (two chains joined by one bridge
/// link, as erroneous LOD links make them), each with a value, beside `groups` small
/// classes of 10. The commit deletes the bridge: the class splits in two.
fn sameas_giant(p: &Params, nt: &mut Nt, commits: &str) -> std::io::Result<Expect> {
    let (size, groups) = (p.get("size", 20_000), p.get("groups", 5_000));
    let same = owl("sameAs");
    let half = size / 2;
    for j in 0..size {
        let x = ex(format!("g{j}"));
        nt.t(&x, &ex("val"), &format!("\"g{j}\""))?;
        if j + 1 < size && j + 1 != half {
            nt.t(&x, &same, &ex(format!("g{}", j + 1)))?;
        }
    }
    let (left, right) = (ex(format!("g{}", half - 1)), ex(format!("g{half}")));
    nt.t(&left, &same, &right)?;
    for g in 0..groups {
        for j in 0..10 {
            let x = ex(format!("s{g}_{j}"));
            nt.t(&x, &ex("val"), &format!("\"s{g}_{j}\""))?;
            if j < 9 {
                nt.t(&x, &same, &ex(format!("s{g}_{}", j + 1)))?;
            }
        }
    }
    let mut out = BufWriter::new(File::create(commits)?);
    writeln!(out, "DELETE DATA {{ {left} {same} {right} }}")?;
    out.flush()?;
    let mut e = Expect::new();
    e.insert("values".into(), json!(size * size + groups * 100));
    e.insert(
        "values_after_split".into(),
        json!(2 * half * half + groups * 100),
    );
    e.insert("giant_values".into(), json!(size));
    Ok(e)
}

/// Two directed graphs on `nodes` nodes with `degree` out-edges each: `u` with uniform
/// targets, `s` with targets skewed towards a few hubs (the high length ratios that make
/// intersections gallop). Their directed triangles, counted here.
fn dense(p: &Params, rng: &mut Rng, nt: &mut Nt) -> std::io::Result<Expect> {
    let (n, degree) = (
        p.get("nodes", 50_000) as usize,
        p.get("degree", 12) as usize,
    );
    let mut e = Expect::new();
    for (name, skewed) in [("u", false), ("s", true)] {
        let mut out: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
        for (a, targets) in out.iter_mut().enumerate() {
            while targets.len() < degree {
                let b = if skewed {
                    rng.skewed(n as u64) as usize
                } else {
                    rng.below(n as u64) as usize
                };
                if b != a {
                    targets.insert(b);
                }
            }
        }
        let predicate = ex(name);
        for (a, targets) in out.iter().enumerate() {
            for &b in targets {
                nt.t(&ex(format!("v{a}")), &predicate, &ex(format!("v{b}")))?;
            }
        }
        // Directed triangles a -> b -> c -> a, each counted once per starting node.
        let mut triangles = 0u64;
        for (a, targets) in out.iter().enumerate() {
            for &b in targets {
                for &c in &out[b] {
                    triangles += u64::from(out[c].contains(&a));
                }
            }
        }
        e.insert(format!("triangles_{name}"), json!(triangles));
    }
    Ok(e)
}

/// The churn schema and people, and commits of growing size: `sizes` people added in one
/// commit each (1, 10, 100, ...), then removed again in one commit each, from the largest.
/// The sweep of delta against store size that decides when maintenance beats
/// rematerialisation (R3-S2's missing experiment).
fn batches(p: &Params, nt: &mut Nt, commits: &str) -> std::io::Result<Expect> {
    let people = p.get("people", 200_000);
    let steps = p.get("steps", 5) as u32;
    // The schema and data are churn's; its own commits are not written.
    let base = Params(BTreeMap::from([
        ("people".to_owned(), people.to_string()),
        ("changes".to_owned(), "0".to_owned()),
    ]));
    churn(&base, nt, &format!("{commits}.unused"))?;
    let _ = std::fs::remove_file(format!("{commits}.unused"));
    let person = |i: u64| {
        format!(
            "<{EX}batch{i}> a <{EX}GraduateStudent> ; <{EX}advisor> <{EX}prof{}> ; <{EX}memberOf> <{EX}dept{}> .",
            i % 500,
            i % 50
        )
    };
    let mut out = BufWriter::new(File::create(commits)?);
    let mut first = 0u64;
    let mut ranges = Vec::new();
    for step in 0..steps {
        let size = 10u64.pow(step);
        ranges.push((first, first + size));
        let body: Vec<String> = (first..first + size).map(person).collect();
        writeln!(out, "INSERT DATA {{ {} }}", body.join(" "))?;
        first += size;
    }
    for &(from, to) in ranges.iter().rev() {
        let body: Vec<String> = (from..to).map(person).collect();
        writeln!(out, "DELETE DATA {{ {} }}", body.join(" "))?;
    }
    out.flush()?;
    let mut e = Expect::new();
    e.insert("people".into(), json!(people));
    e.insert("commits".into(), json!(2 * steps));
    Ok(e)
}

/// Equality added to LUBM (loaded beside it): `one=1` asserts one `owl:sameAs` between two
/// of LUBM 10's graduate students; `depth=D` adds `chains` functional-property cascades of
/// depth D: `a fp b1`, `a fp c1` equate b1 and c1, whose own `fp` values then equate, D
/// levels down, so each merge triggers the next.
fn eq_addon(p: &Params, nt: &mut Nt) -> std::io::Result<Expect> {
    let (one, depth, chains) = (p.get("one", 0), p.get("depth", 0), p.get("chains", 1000));
    let lubm = |local: &str| format!("<http://www.Department0.University0.edu/{local}>");
    if one > 0 {
        nt.t(
            &lubm("GraduateStudent1"),
            &owl("sameAs"),
            &lubm("GraduateStudent2"),
        )?;
    }
    let fp = ex("fp");
    if depth > 0 {
        nt.t(&fp, &rdf("type"), &owl("FunctionalProperty"))?;
    }
    for c in 0..if depth > 0 { chains } else { 0 } {
        nt.t(&ex(format!("a{c}")), &fp, &ex(format!("b{c}_1")))?;
        nt.t(&ex(format!("a{c}")), &fp, &ex(format!("c{c}_1")))?;
        for d in 1..depth {
            nt.t(
                &ex(format!("b{c}_{d}")),
                &fp,
                &ex(format!("b{c}_{}", d + 1)),
            )?;
            nt.t(
                &ex(format!("c{c}_{d}")),
                &fp,
                &ex(format!("c{c}_{}", d + 1)),
            )?;
        }
    }
    let mut e = Expect::new();
    // Each cascade equates b_d with c_d at every depth: `depth` classes of two per chain,
    // and the asserted pair: sameAs pairs (reflexive included) are 4 per class.
    e.insert("same_as_pairs".into(), json!(4 * depth * chains + 4 * one));
    Ok(e)
}

/// A transitive property `p` fed over several rounds: of each chain's edges, a third are
/// asserted as `p`, a third as its inverse `r`, and a third through a property chain
/// `h ∘ h ⊑ p` whose `h` edges come from `g ⊑ h` and the inverse of `hInv`. The modules
/// then close `p` again in each round that rules add to it.
fn fed_chain(p: &Params, nt: &mut Nt) -> std::io::Result<Expect> {
    let (chains, length) = (p.get("chains", 2), p.get("length", 1500));
    let (pp, r, h) = (ex("p"), ex("r"), ex("h"));
    nt.t(&pp, &rdf("type"), &owl("TransitiveProperty"))?;
    nt.t(&r, &owl("inverseOf"), &pp)?;
    nt.t(&ex("g"), &rdfs("subPropertyOf"), &h)?;
    nt.t(&ex("hInv"), &owl("inverseOf"), &h)?;
    let chain = list(nt, "hh", &[h.clone(), h.clone()])?;
    nt.t(&pp, &owl("propertyChainAxiom"), &chain)?;
    for c in 0..chains {
        let n = |i: u64| ex(format!("n{c}_{i}"));
        for i in 0..length {
            let (a, b) = (n(i), n(i + 1));
            match i % 3 {
                0 => nt.t(&a, &pp, &b)?,
                1 => nt.t(&b, &r, &a)?,
                _ => {
                    let m = ex(format!("m{c}_{i}"));
                    nt.t(&a, &ex("g"), &m)?;
                    nt.t(&b, &ex("hInv"), &m)?;
                }
            }
        }
    }
    let mut e = Expect::new();
    e.insert("pairs".into(), json!(chains * length * (length + 1) / 2));
    Ok(e)
}
