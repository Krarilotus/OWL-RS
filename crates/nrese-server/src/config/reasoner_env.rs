use anyhow::{Result, bail};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};

use super::env_names as names;
use super::source::ConfigSource;

pub(super) fn parse_reasoner_config(source: &dyn ConfigSource) -> Result<ReasonerConfig> {
    let mode = parse_reasoning_mode(source.get(names::REASONING_MODE).as_deref())?;
    Ok(ReasonerConfig::for_mode(mode))
}

/// Unknown values are a startup error: silently falling back to `disabled` would turn a
/// typo into "no consistency checking at all".
fn parse_reasoning_mode(input: Option<&str>) -> Result<ReasoningMode> {
    let name = input
        .unwrap_or("disabled")
        .to_ascii_lowercase()
        .replace('_', "-");
    if let Some(mode) = ReasoningMode::from_name(&name) {
        return Ok(mode);
    }
    match name.as_str() {
        "none" | "off" => Ok(ReasoningMode::Disabled),
        "owl2rl" => Ok(ReasoningMode::Owl2Rl),
        "owl2ql" => Ok(ReasoningMode::Owl2Ql),
        "rulesmvp" | "rules-mvp" => bail!(
            "{}: the rules-mvp reasoner was removed; use 'owl2-rl' (OWL 2 RL, with consistency \
             checks) or 'rdfs'",
            names::REASONING_MODE
        ),
        unknown => bail!(
            "unsupported value '{unknown}' in {} (expected 'disabled', 'rdfs', 'rdfs-full', \
             'rdfs-plus', 'owl-horst', 'owl2-ql' or 'owl2-rl')",
            names::REASONING_MODE
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_reasoning_mode;

    #[test]
    fn reasoning_mode_rejects_unknown_values_instead_of_disabling() {
        assert!(parse_reasoning_mode(Some("owl-dl-target")).is_err());
        assert!(parse_reasoning_mode(Some("rules-mvp")).is_err());
        assert_eq!(
            parse_reasoning_mode(None).expect("default"),
            nrese_reasoner::ReasoningMode::Disabled
        );
        assert_eq!(
            parse_reasoning_mode(Some("OWL2-RL")).expect("owl2-rl"),
            nrese_reasoner::ReasoningMode::Owl2Rl
        );
        assert_eq!(
            parse_reasoning_mode(Some("rdfs")).expect("rdfs"),
            nrese_reasoner::ReasoningMode::Rdfs
        );
        for mode in nrese_reasoner::ReasoningMode::REASONING {
            assert_eq!(parse_reasoning_mode(Some(mode.as_str())).unwrap(), mode);
        }
        assert_eq!(
            parse_reasoning_mode(Some("OWL_HORST")).unwrap(),
            nrese_reasoner::ReasoningMode::OwlHorst
        );
    }
}
