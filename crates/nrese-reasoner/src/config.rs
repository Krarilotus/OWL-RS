use crate::v2::rulesets::Ruleset;

/// The reasoning mode of a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningMode {
    /// No reasoning: reads see asserted statements only.
    #[default]
    Disabled,
    /// RDFS: inferences materialised into the inferred stack.
    Rdfs,
    /// The OWL 2 RL/RDF rules: inferences materialised into the inferred stack, and
    /// consistency violations reject the commits that cause them.
    Owl2Rl,
}

impl ReasoningMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Rdfs => "rdfs",
            Self::Owl2Rl => "owl2-rl",
        }
    }

    /// The ruleset the store materialises, if any.
    pub const fn ruleset(self) -> Option<Ruleset> {
        match self {
            Self::Disabled => None,
            Self::Rdfs => Some(Ruleset::Rdfs),
            Self::Owl2Rl => Some(Ruleset::Owl2Rl),
        }
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
            ReasoningMode::Rdfs | ReasoningMode::Owl2Rl => "materialised",
        }
    }
}
