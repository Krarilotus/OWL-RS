//! One solution: a value (or none) per variable, the variables shared by all solutions.

use std::fmt;
use std::sync::Arc;

use nrese_rdf::{Term, Variable};

/// A solution of a `SELECT` query: the value of each variable, if it is bound.
#[derive(Clone, PartialEq, Eq)]
pub struct QuerySolution {
    variables: Arc<[Variable]>,
    values: Vec<Option<Term>>,
}

impl QuerySolution {
    /// The value of a variable: by name (without `?`), by [`Variable`], or by its index in
    /// [`Self::variables`].
    pub fn get(&self, variable: impl SolutionIndex) -> Option<&Term> {
        let i = variable.index(&self.variables)?;
        self.values.get(i)?.as_ref()
    }

    /// The value of the variable at `index` in [`Self::variables`].
    pub fn get_index(&self, index: usize) -> Option<&Term> {
        self.values.get(index)?.as_ref()
    }

    /// The number of variables (bound or not).
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The bound variables and their values.
    pub fn iter(&self) -> impl Iterator<Item = (&Variable, &Term)> {
        self.variables
            .iter()
            .zip(&self.values)
            .filter_map(|(variable, value)| Some((variable, value.as_ref()?)))
    }

    /// One value or `None` per variable, in the order of [`Self::variables`].
    pub fn values(&self) -> &[Option<Term>] {
        &self.values
    }

    pub fn variables(&self) -> &[Variable] {
        &self.variables
    }
}

/// What picks a variable of a solution: its name, the variable, or its index.
pub trait SolutionIndex {
    fn index(self, variables: &[Variable]) -> Option<usize>;
}

impl SolutionIndex for usize {
    fn index(self, variables: &[Variable]) -> Option<usize> {
        (self < variables.len()).then_some(self)
    }
}

impl SolutionIndex for &str {
    fn index(self, variables: &[Variable]) -> Option<usize> {
        variables.iter().position(|v| v.as_str() == self)
    }
}

impl SolutionIndex for &Variable {
    fn index(self, variables: &[Variable]) -> Option<usize> {
        self.as_str().index(variables)
    }
}

impl<V: Into<Arc<[Variable]>>, S: Into<Vec<Option<Term>>>> From<(V, S)> for QuerySolution {
    fn from((variables, values): (V, S)) -> Self {
        Self {
            variables: variables.into(),
            values: values.into(),
        }
    }
}

impl<'a> IntoIterator for &'a QuerySolution {
    type Item = (&'a Variable, &'a Term);
    type IntoIter = Box<dyn Iterator<Item = (&'a Variable, &'a Term)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl fmt::Debug for QuerySolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.iter().map(|(v, t)| (v.as_str(), t.to_string())))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use nrese_rdf::NamedNode;

    use super::*;

    #[test]
    fn access_by_name_and_index() {
        let variables: Arc<[Variable]> =
            vec![Variable::new_unchecked("a"), Variable::new_unchecked("b")].into();
        let value: Term = NamedNode::new_unchecked("http://e/x").into();
        let solution = QuerySolution::from((variables, vec![None, Some(value.clone())]));
        assert_eq!(solution.get("b"), Some(&value));
        assert_eq!(solution.get("a"), None);
        assert_eq!(solution.get("c"), None);
        assert_eq!(solution.get_index(1), Some(&value));
        assert_eq!(solution.iter().count(), 1);
        assert_eq!(solution.len(), 2);
    }
}
