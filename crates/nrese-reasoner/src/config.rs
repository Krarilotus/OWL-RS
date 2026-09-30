use crate::v2::rulesets::Ruleset;

/// The reasoning mode of a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningMode {
    /// No reasoning: reads see asserted statements only.
    #[default]
    Disabled,
    /// RDFS: inferences materialised into the inferred stack.
    Rdfs,
    /// Every RDFS rule and the axiomatic triples.
    RdfsFull,
    /// RDFS-Plus.
    RdfsPlus,
    /// OWL-Horst (pD*).
    OwlHorst,
    /// OWL 2 QL, materialised; its consistency checks reject commits.
    Owl2Ql,
    /// The OWL 2 RL/RDF rules: inferences materialised into the inferred stack, and
    /// consistency violations reject the commits that cause them.
    Owl2Rl,
}

impl ReasoningMode {
    /// Every mode that reasons, one per ruleset.
    pub const REASONING: [Self; 6] = [
        Self::Rdfs,
        Self::RdfsFull,
        Self::RdfsPlus,
        Self::OwlHorst,
        Self::Owl2Ql,
        Self::Owl2Rl,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Rdfs => "rdfs",
            Self::RdfsFull => "rdfs-full",
            Self::RdfsPlus => "rdfs-plus",
            Self::OwlHorst => "owl-horst",
            Self::Owl2Ql => "owl2-ql",
            Self::Owl2Rl => "owl2-rl",
        }
    }

    /// The ruleset the store materialises, if any.
    pub const fn ruleset(self) -> Option<Ruleset> {
        match self {
            Self::Disabled => None,
            Self::Rdfs => Some(Ruleset::Rdfs),
            Self::RdfsFull => Some(Ruleset::RdfsFull),
            Self::RdfsPlus => Some(Ruleset::RdfsPlus),
            Self::OwlHorst => Some(Ruleset::OwlHorst),
            Self::Owl2Ql => Some(Ruleset::Owl2Ql),
            Self::Owl2Rl => Some(Ruleset::Owl2Rl),
        }
    }

    /// The mode that materialises `ruleset`.
    pub fn for_ruleset(ruleset: Ruleset) -> Self {
        Self::REASONING
            .into_iter()
            .find(|mode| mode.ruleset() == Some(ruleset))
            .expect("a mode per ruleset")
    }

    /// The mode named `name` ([`ReasoningMode::as_str`]).
    pub fn from_name(name: &str) -> Option<Self> {
        std::iter::once(Self::Disabled)
            .chain(Self::REASONING)
            .find(|mode| mode.as_str() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReasonerConfig {
    pub mode: ReasoningMode,
}

impl ReasonerConfig {
    pub const fn for_mode(mode: ReasoningMode) -> Self {
        Self { mode }
    }

    pub const fn mode(&self) -> ReasoningMode {
        self.mode
    }

    /// The ruleset whose closure the store materialises, if any.
    pub const fn materialised_ruleset(&self) -> Option<Ruleset> {
        self.mode.ruleset()
    }

    /// Which statements reads see: `materialised` (asserted and inferred) with reasoning,
    /// `asserted-only` without.
    pub const fn read_model_name(&self) -> &'static str {
        match self.mode {
            ReasoningMode::Disabled => "asserted-only",
            _ => "materialised",
        }
    }
}
