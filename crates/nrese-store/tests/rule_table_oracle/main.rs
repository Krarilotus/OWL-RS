mod cells;
mod cycle_compat;
mod generator;
mod judges;
mod oracle;
mod per_cell;
mod publication;
mod review_cells;
mod sweep;

type Triple = [String; 3];
type Facts = std::collections::BTreeSet<Triple>;

fn triple(terms: [&str; 3]) -> Triple {
    terms.map(str::to_owned)
}
