//! Tests over nested expressions decided with a stack of their own, so that an expression's
//! nesting depth is no call depth.

/// What one expression says to [`holds`]: a value of its own, or that all (or any) of the
/// given operands must hold, each under `R`.
pub(crate) enum Holds<'a, Id, R> {
    Is(bool),
    All(&'a [Id], R),
    Any(&'a [Id], R),
}

/// Whether `root` holds under `rule`, `step` saying what each expression needs. The operands
/// are decided from the left with the short-circuits of `&&` and `||`, as nested calls of
/// `step` would decide them.
pub(crate) fn holds<'a, Id: Copy + 'a, R: Copy>(
    root: Id,
    rule: R,
    mut step: impl FnMut(Id, R) -> Holds<'a, Id, R>,
) -> bool {
    // The open conjunctions (`true`) and disjunctions, each with its operands still to decide.
    let mut open: Vec<(bool, std::slice::Iter<'a, Id>, R)> = Vec::new();
    let mut next = (root, rule);
    loop {
        let mut value = match step(next.0, next.1) {
            Holds::Is(value) => Some(value),
            Holds::All(operands, rule) => {
                open.push((true, operands.iter(), rule));
                None
            }
            Holds::Any(operands, rule) => {
                open.push((false, operands.iter(), rule));
                None
            }
        };
        loop {
            let Some((all, operands, rule)) = open.last_mut() else {
                return value.expect("the root is decided");
            };
            // A false operand decides a conjunction, a true one a disjunction.
            if value == Some(!*all) {
                open.pop();
                continue;
            }
            if let Some(&operand) = operands.next() {
                next = (operand, *rule);
                break;
            }
            value = Some(*all);
            open.pop();
        }
    }
}
