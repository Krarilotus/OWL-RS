use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use nrese_rdf::Quad;
use nrese_sparql::CancellationToken;

use super::case::Case;
use super::random::{Verdict, check};

const ATTEMPTS: usize = 400;
const BUDGET: Duration = Duration::from_secs(20);

pub(super) struct Shrunk {
    pub case: Case,
    pub attempts: usize,
    pub ran_out: bool,
}

impl Shrunk {
    /// Runs `attempt` and keeps it if it shows the same signature, within the budget.
    fn keep(&mut self, attempt: Case, signature: &str, start: Instant) -> bool {
        if self.attempts >= ATTEMPTS || start.elapsed() >= BUDGET {
            self.ran_out = true;
            return false;
        }
        self.attempts += 1;
        // Only draws are shrunk, and no entry names a draw.
        let same =
            matches!(check(&attempt, "", &[], true), Verdict::Fail(shown) if shown == signature);
        if same {
            self.case = attempt;
        }
        same
    }
}

/// The statement lists: the start state's asserted and inferred, then each step's four.
fn statements(case: &mut Case) -> Vec<&mut Vec<Quad>> {
    let mut lists = vec![&mut case.asserted, &mut case.inferred];
    for step in &mut case.steps {
        lists.extend([
            &mut step.add,
            &mut step.remove,
            &mut step.infer,
            &mut step.retract,
        ]);
    }
    lists
}

/// The shapes texts: a shape is one line, a constraint one `;`-separated part after its first.
fn shapes(case: &mut Case) -> Vec<&mut String> {
    let mut texts: Vec<_> = case.shapes.iter_mut().collect();
    texts.extend(
        case.steps
            .iter_mut()
            .filter_map(|step| step.shapes.as_mut()),
    );
    texts
}

fn parts(line: &str) -> Vec<&str> {
    line.trim_end()
        .trim_end_matches(" .")
        .split(" ; ")
        .collect()
}

fn remove_line(text: &mut String, i: usize) {
    let shorter = {
        let mut lines: Vec<_> = text.lines().collect();
        lines.remove(i);
        lines.join("\n")
    };
    *text = shorter;
}

fn remove_part(text: &mut String, line: usize, i: usize) {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let shorter = {
        let mut kept = parts(&lines[line]);
        kept.remove(i + 1);
        format!("{} .", kept.join(" ; "))
    };
    lines[line] = shorter;
    *text = lines.join("\n");
}

/// The smallest case found that shows `signature`: steps, then statements, shapes and
/// constraints, each removed greedily. The deadline also stops a running attempt.
pub(super) fn shrink(case: &Case, signature: &str) -> Shrunk {
    let token = CancellationToken::new();
    let (finished, deadline) = mpsc::channel::<()>();
    let timer = {
        let token = token.clone();
        thread::spawn(move || {
            if let Err(RecvTimeoutError::Timeout) = deadline.recv_timeout(BUDGET) {
                token.cancel();
            }
        })
    };
    let start = Instant::now();
    let mut s = Shrunk {
        case: case.clone(),
        attempts: 0,
        ran_out: false,
    };
    s.case.cancellation = Some(token);
    // Removes item `i` of `count` items while the signature holds, from the first on.
    type Count<'c> = &'c dyn Fn(&mut Case) -> usize;
    type Remove<'c> = &'c dyn Fn(&mut Case, usize);
    let pass = |s: &mut Shrunk, count: Count<'_>, remove: Remove<'_>| {
        let mut i = 0;
        while !s.ran_out && i < count(&mut s.case) {
            let mut attempt = s.case.clone();
            remove(&mut attempt, i);
            if !s.keep(attempt, signature, start) {
                i += 1;
            }
        }
    };
    // A case keeps at least one step.
    let steps = |c: &mut Case| if c.steps.len() > 1 { c.steps.len() } else { 0 };
    pass(&mut s, &steps, &|c: &mut Case, i: usize| {
        drop(c.steps.remove(i))
    });
    for list in 0..statements(&mut s.case).len() {
        let count = |c: &mut Case| statements(c)[list].len();
        let remove = |c: &mut Case, i: usize| drop(statements(c).swap_remove(list).remove(i));
        pass(&mut s, &count, &remove);
    }
    let texts = shapes(&mut s.case).len();
    for text in 0..texts {
        let count = |c: &mut Case| shapes(c)[text].lines().count();
        let remove = |c: &mut Case, i: usize| remove_line(shapes(c).swap_remove(text), i);
        pass(&mut s, &count, &remove);
    }
    for text in 0..texts {
        let mut line = 0;
        while line < shapes(&mut s.case)[text].lines().count() {
            let count = |c: &mut Case| {
                let texts = shapes(c);
                parts(texts[text].lines().nth(line).unwrap_or_default()).len() - 1
            };
            let remove = |c: &mut Case, i: usize| remove_part(shapes(c).swap_remove(text), line, i);
            pass(&mut s, &count, &remove);
            line += 1;
        }
    }
    drop(finished);
    timer.join().expect("shrink timer");
    s.case.cancellation = None;
    s
}
