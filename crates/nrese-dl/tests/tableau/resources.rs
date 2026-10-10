use nrese_dl::tableau::{self, Answer, Cancel, Config};
use std::time::Duration;

#[test]
fn cancellation_and_expiry_precede_compilation() {
    let ontology = super::multiplication(2, 3, 6, true);
    let normalised = nrese_owl::normalise(&ontology);
    let cancel = Cancel::default();
    cancel.cancel();
    for config in [
        Config {
            cancel: Some(cancel),
            ..Config::default()
        },
        Config {
            timeout: Some(Duration::ZERO),
            ..Config::default()
        },
    ] {
        for keep_model in [false, true] {
            let config = Config {
                keep_model,
                ..config.clone()
            };
            for outcome in [
                tableau::consistency(&ontology, &config),
                tableau::consistency_of(&ontology, &normalised, &config),
                tableau::satisfiable(&ontology, &normalised, 1, &config),
            ] {
                assert!(
                    matches!(outcome.answer, Answer::GaveUp(_)),
                    "{:?}",
                    outcome.answer
                );
                assert_eq!(outcome.telemetry.compile, Duration::ZERO);
                assert_eq!(outcome.telemetry.peak_nodes, 0);
                assert!(outcome.model.is_none());
            }
        }
    }
}
