//! The guard of `nrese_dl::limits`: no DL code path applies a budget its configuration
//! didn't set. The engines' defaults set none; each budget, set small, stops a run with a
//! reason that names its setting, and the same run decides with nothing set; and no
//! literal budget hides in the DL sources (a scan of the engines, their CLIs and the
//! store's DL drivers, the constants that bound no run listed with what they are).

use std::path::{Path, PathBuf};
use std::time::Duration;

use nrese_dl::limits::Limit;
use nrese_dl::tableau::{Answer, Config, consistency};
use nrese_owl::{ClassExpr, ObjProp};

use super::build::Build;

#[test]
fn the_engines_set_no_budget_by_default() {
    let t = Config::default();
    assert_eq!((t.timeout, t.max_memory, t.max_nodes), (None, None, None));
    assert_eq!((t.max_branch_points, t.max_merges), (None, None));
    assert!(t.memory.is_none());
    assert_eq!(
        nrese_dl::context::Budget::default(),
        nrese_dl::context::Budget {
            deadline: None,
            max_join: None,
            max_join_steps: None,
            max_memory: None,
            task_memory: None,
            memory: None,
        }
    );
    let o = nrese_dl::classify::Options::default();
    assert_eq!((o.timeout, o.max_memory, o.task_memory), (None, None, None));
    assert!(
        o.tableau == Config::default(),
        "a test's budgets are its caller's"
    );
}

/// `a : C` with `C ⊑ ∃r.D ⊓ (E ⊔ F)`, and `a` at most one `r`-successor with two named
/// ones: nodes, a branch point and merges, consistent.
fn case() -> Build {
    let mut b = Build::default();
    let (c, d, e, f) = (b.class(1), b.class(2), b.class(3), b.class(4));
    let r = ObjProp::Named(10);
    let some = b.some(r, d);
    let either = b.or(&[e, f]);
    let both = b.and(&[some, either]);
    b.sub(c, both);
    b.assert(c, 100);
    let thing = b.e(ClassExpr::Thing);
    let at_most_one = b.e(ClassExpr::Max(1, r, thing));
    b.assert(at_most_one, 100);
    b.role_assertion(10, 100, 101);
    b.role_assertion(10, 100, 102);
    b
}

fn base() -> Config {
    Config {
        portfolio: false,
        ..Config::default()
    }
}

#[test]
fn each_budget_reached_names_its_setting_and_none_set_decides() {
    let o = case().o;
    assert_eq!(consistency(&o, &base()).answer, Answer::Consistent);
    let tiny = nrese_exec::Envelope::new(Some(1), None);
    let set: [(Limit, Config); 6] = [
        (
            Limit::Time,
            Config {
                timeout: Some(Duration::ZERO),
                ..base()
            },
        ),
        (
            Limit::Memory,
            Config {
                max_memory: Some(1),
                ..base()
            },
        ),
        (
            Limit::Envelope,
            Config {
                memory: Some(tiny),
                ..base()
            },
        ),
        (
            Limit::Nodes,
            Config {
                max_nodes: Some(1),
                ..base()
            },
        ),
        (
            Limit::BranchPoints,
            Config {
                max_branch_points: Some(0),
                ..base()
            },
        ),
        (
            Limit::Merges,
            Config {
                max_merges: Some(0),
                ..base()
            },
        ),
    ];
    for (limit, config) in set {
        match consistency(&o, &config).answer {
            Answer::GaveUp(why) => assert_eq!(Limit::named_in(&why), Some(limit), "{why}"),
            other => panic!("{limit:?} set small, and the run answered {other:?}"),
        }
    }
    // A classification reports a test's budget by its setting too.
    let options = nrese_dl::classify::Options {
        tableau: Config {
            max_nodes: Some(1),
            ..Config::default()
        },
        ..nrese_dl::classify::Options::default()
    };
    let taxonomy = nrese_dl::classify::classify(&o, &options);
    assert!(
        taxonomy
            .incomplete
            .iter()
            .all(|why| Limit::named_in(why).is_some()),
        "{:?}",
        taxonomy.incomplete
    );
}

/// Constants and literals of the DL sources that bound no run, and what each is: a
/// representation's capacity, a batch or interval of the work, a display, a default a
/// setting documents (docs/ops/config-reference.md, `dl.*`).
const NOT_BUDGETS: &[(&str, &str, &str)] = &[
    (
        "bounds/project.rs",
        "MAX_BODY",
        "atoms per rule of U1, folded exactly",
    ),
    (
        "bounds/upper.rs",
        "MAX_HEAD",
        "atoms per rule head of U1, split exactly",
    ),
    ("bounds/upper.rs", "MIN_SKOLEMS", "U1's compilation option"),
    (
        "classify/inline.rs",
        "PASSES",
        "inlining rounds, an optimisation",
    ),
    (
        "classify/known.rs",
        "RETRIES",
        "the lower bound's retries, a route threshold",
    ),
    ("context/atoms.rs", "MAX_ID", "the atom id space"),
    ("context/engine.rs", "QUEUED", "an inbox's batch"),
    ("context/engine.rs", "BASE", "the arena's first segment"),
    ("context/engine.rs", "SEGMENTS", "the arena's segments"),
    (
        "datatypes/theory.rs",
        "STOP_EVERY",
        "how often the theory checks its stop",
    ),
    ("islands.rs", "BATCH", "assertions per island batch"),
    (
        "tableau/depset.rs",
        "MEMO_LIMIT",
        "a cache of dependency-set unions",
    ),
    (
        "tableau/program.rs",
        "MAX_VARS",
        "variables per clause in a join's bindings",
    ),
    ("tableau/graph.rs", "pub const", "a node's flag bits"),
    ("tableau/search.rs", "WINDOW", "the restart window"),
    (
        "dl/source.rs",
        "FRESH",
        "fresh individuals the entailment tests use",
    ),
    (
        "dl/mod.rs",
        "UNRESOLVED_SHOWN",
        "unresolved candidates a status names",
    ),
    (
        "dl/gate.rs",
        "from_millis(10)",
        "the cancellation bridge's poll interval",
    ),
    (
        "classify/mod.rs",
        "lower_bound_timeout: Some(Duration::from_secs(60))",
        "dl.lower_bound_timeout's default, a route threshold",
    ),
    (
        "classify/mod.rs",
        "max_join: Some(1 << 20)",
        "dl.max_join's default",
    ),
    (
        "classify/mod.rs",
        "max_join_steps: Some(1 << 16)",
        "dl.max_join_steps' default",
    ),
    (
        "dl/config.rs",
        "Duration::from_secs(30 * 60)",
        "dl.timeout's default",
    ),
    (
        "dl/config.rs",
        "max_candidates: Some(1_000)",
        "dl.max_candidates' default",
    ),
];

/// Non-test lines of `text`: an item under `#[cfg(test)]` is left out.
fn product_lines(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let (mut skipping, mut depth, mut opened) = (false, 0i64, false);
    for (i, line) in text.lines().enumerate() {
        let code = line.trim();
        if code.starts_with("#[cfg(test)]") {
            skipping = true;
            (depth, opened) = (0, false);
            continue;
        }
        if skipping {
            if code.starts_with("#[") {
                continue;
            }
            depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
            opened |= line.contains('{');
            if (opened && depth <= 0) || (!opened && code.ends_with(';')) {
                skipping = false;
            }
            continue;
        }
        if !code.starts_with("//") {
            out.push((i + 1, line));
        }
    }
    out
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                sources(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            out.push(path);
        }
    }
}

/// Whether `line` sets a budget to a literal: a constant of a size or time, a time, or a
/// budget field given a number.
fn literal_budget(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or("");
    // A sentinel (`u32::MAX`) or a combination of flags is no limit.
    let constant = code.contains("const ")
        && [": usize", ": u64", ": u32", ": Duration"]
            .iter()
            .any(|t| code.contains(t))
        && !code.contains("::MAX")
        && !code.contains('|');
    let time = ["from_secs(", "from_millis(", "from_micros(", "from_nanos("]
        .iter()
        .any(|f| {
            code.split(f)
                .skip(1)
                .any(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        });
    let field = [
        "max_memory: ",
        "max_nodes: ",
        "max_branch_points: ",
        "max_merges: ",
        "max_join: ",
        "max_join_steps: ",
        "max_candidates: ",
        "memory_bytes: ",
        "task_memory: ",
        "explanation_tests: ",
        "timeout: ",
    ]
    .iter()
    .any(|f| {
        code.split(f).skip(1).any(|rest| {
            let rest = rest.trim_start_matches("Some(");
            let rest = rest.trim_start_matches("std::time::");
            let rest = ["Duration::from_secs(", "Duration::from_millis("]
                .iter()
                .find_map(|f| rest.strip_prefix(f))
                .unwrap_or(rest);
            rest.starts_with(|c: char| c.is_ascii_digit())
        })
    });
    constant || time || field
}

#[test]
fn no_literal_budget_hides_in_the_dl_sources() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["src", "examples", "../nrese-store/src/dl"] {
        sources(&crate_dir.join(dir), &mut files);
    }
    let mut found = Vec::new();
    let mut allowed = vec![false; NOT_BUDGETS.len()];
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let shown = path.to_string_lossy().replace('\\', "/");
        for (number, line) in product_lines(&text) {
            if !literal_budget(line) {
                continue;
            }
            match NOT_BUDGETS
                .iter()
                .position(|(file, needle, _)| shown.ends_with(file) && line.contains(needle))
            {
                Some(at) => allowed[at] = true,
                None => found.push(format!("{shown}:{number}: {}", line.trim())),
            }
        }
    }
    assert!(
        found.is_empty(),
        "literal budgets in the DL sources (make each a setting, or list what it is in \
         NOT_BUDGETS):\n{}",
        found.join("\n")
    );
    let stale: Vec<_> = NOT_BUDGETS
        .iter()
        .zip(&allowed)
        .filter(|(_, used)| !**used)
        .map(|((file, needle, _), _)| format!("{file}: {needle}"))
        .collect();
    assert!(
        stale.is_empty(),
        "NOT_BUDGETS lists what isn't there: {stale:?}"
    );
}
