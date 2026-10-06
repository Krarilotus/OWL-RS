use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use nrese_reasoner::{ReasonerConfig, ReasoningMode, UserRules};

use super::env_names as names;
use super::source::ConfigSource;

pub(super) fn parse_reasoner_config(source: &dyn ConfigSource) -> Result<ReasonerConfig> {
    let mode = parse_reasoning_mode(source.get(names::REASONING_MODE).as_deref())?;
    let rules = match source
        .get(names::REASONING_RULES)
        .filter(|p| !p.trim().is_empty())
    {
        Some(path) => Some(Arc::new(load_rules(Path::new(path.trim()))?)),
        None => None,
    };
    ReasonerConfig::for_mode(mode)
        .with_rules(rules)
        .with_context(|| format!("{} and {}", names::REASONING_MODE, names::REASONING_RULES))
}

/// The user's rules from `path`, checked now: a mistake stops the startup, with the
/// rule it is in.
fn load_rules(path: &Path) -> Result<UserRules> {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase());
    let pie = match extension.as_deref() {
        Some("n3") => false,
        Some("pie") => true,
        _ => bail!(
            "{}: {} is neither a Notation3 file (.n3) nor a GraphDB ruleset (.pie), the \
             rule formats NRESE reads",
            names::REASONING_RULES,
            path.display()
        ),
    };
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("{}: reading {}", names::REASONING_RULES, path.display()))?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let rules = if pie {
        UserRules::pie(name, text)
    } else {
        UserRules::n3(name, text)
    };
    rules.with_context(|| format!("{}: in {}", names::REASONING_RULES, path.display()))
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
        "owl2dl" => Ok(ReasoningMode::Owl2Dl),
        "rulesmvp" | "rules-mvp" => bail!(
            "{}: the rules-mvp reasoner was removed; use 'owl2-rl' (OWL 2 RL, with consistency \
             checks) or 'rdfs'",
            names::REASONING_MODE
        ),
        unknown => bail!(
            "unsupported value '{unknown}' in {} (expected 'disabled', 'rdfs', 'rdfs-full', \
             'rdfs-plus', 'owl-horst', 'owl2-ql', 'owl2-rl', 'owl2-dl' or 'custom')",
            names::REASONING_MODE
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::source::KeyValueSource;
    use super::{names, parse_reasoner_config, parse_reasoning_mode};

    #[test]
    fn user_rules_load_with_the_mode_they_need() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rules = dir.path().join("family.n3");
        std::fs::write(
            &rules,
            "@prefix : <http://e/> . { ?x :parent ?y . ?y :parent ?z } => { ?x :grandparent ?z } .",
        )
        .expect("rules file");
        let config = |mode: &str, path: &std::path::Path| {
            let mut source = KeyValueSource::default();
            source.insert(names::REASONING_MODE, mode);
            source.insert(names::REASONING_RULES, path.to_string_lossy().into_owned());
            parse_reasoner_config(&source)
        };
        let custom = config("custom", &rules).expect("custom");
        assert_eq!(
            custom.materialised_program().expect("a program").name(),
            "custom:family.n3"
        );
        let added = config("owl2-rl", &rules).expect("owl2-rl with rules");
        assert_eq!(
            added.materialised_program().expect("a program").name(),
            "owl2-rl+custom:family.n3"
        );
        assert!(config("disabled", &rules).is_err());
        let wrong = dir.path().join("family.rules");
        std::fs::write(&wrong, "").expect("file");
        assert!(config("custom", &wrong).is_err());
        let broken = dir.path().join("broken.n3");
        std::fs::write(
            &broken,
            "{ ?x <http://e/p> ?y } => { ?x <http://e/q> [] } .",
        )
        .expect("file");
        let error = format!("{:#}", config("custom", &broken).unwrap_err());
        assert!(error.contains("broken.n3"), "{error}");
        let mut source = KeyValueSource::default();
        source.insert(names::REASONING_MODE, "custom");
        assert!(parse_reasoner_config(&source).is_err());
    }

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
        assert_eq!(
            parse_reasoning_mode(Some("owl2-dl")).unwrap(),
            nrese_reasoner::ReasoningMode::Owl2Dl
        );
    }
}
