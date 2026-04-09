use crate::runtime_posture::{
    GRAPH_STORE_ENDPOINT, METRICS_ENDPOINT, OPERATOR_UI_PATH, QUERY_ENDPOINT,
    SERVICE_DESCRIPTION_ENDPOINT, TELL_ENDPOINT, UPDATE_ENDPOINT,
};
use crate::state::AppState;

pub fn build_service_description(state: &AppState) -> String {
    let posture = state.runtime_posture();
    let reasoning_mode = state.reasoner_mode_name();
    let reasoning_profile = state.reasoner_profile_name();
    let graph_store_enabled = bool_literal(posture.graph_store_enabled);
    let sparql_update_enabled = bool_literal(posture.sparql_update_enabled);
    let tell_enabled = bool_literal(posture.tell_enabled);
    let federated_service_enabled = "false";
    let metrics_endpoint = posture
        .metrics_path()
        .map(|_| format!("   nrese:metricsEndpoint <{METRICS_ENDPOINT}> ;\n"))
        .unwrap_or_default();
    let operator_endpoint = posture
        .operator_ui_path()
        .map(|_| format!("   nrese:operatorEndpoint <{OPERATOR_UI_PATH}> ;\n"))
        .unwrap_or_default();

    format!(
        "@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .\n\
@prefix format: <http://www.w3.org/ns/formats/> .\n\
@prefix void: <http://rdfs.org/ns/void#> .\n\
@prefix nrese: <https://nrese.dev/ns/service#> .\n\
\n\
[] a sd:Service ;\n\
   sd:endpoint <{QUERY_ENDPOINT}> ;\n\
   sd:supportedLanguage sd:SPARQL11Query ;\n\
   sd:resultFormat format:SPARQL_Results_JSON , format:SPARQL_Results_XML , format:SPARQL_Results_CSV , format:SPARQL_Results_TSV ;\n\
   nrese:serviceDescriptionEndpoint <{SERVICE_DESCRIPTION_ENDPOINT}> ;\n\
   nrese:updateEndpoint <{UPDATE_ENDPOINT}> ;\n\
   nrese:tellEndpoint <{TELL_ENDPOINT}> ;\n\
   nrese:graphStoreEndpoint <{GRAPH_STORE_ENDPOINT}> ;\n\
{metrics_endpoint}\
{operator_endpoint}\
   nrese:reasoningMode \"{reasoning_mode}\" ;\n\
   nrese:reasoningProfile \"{reasoning_profile}\" ;\n\
   nrese:graphStoreEnabled \"{graph_store_enabled}\" ;\n\
   nrese:sparqlUpdateEnabled \"{sparql_update_enabled}\" ;\n\
   nrese:tellEnabled \"{tell_enabled}\" ;\n\
   nrese:federatedServiceEnabled \"{federated_service_enabled}\" .\n"
    )
}

fn bool_literal(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[cfg(test)]
mod tests {
    use nrese_reasoner::{ReasonerConfig, ReasonerService};
    use nrese_store::{StoreConfig, StoreService};

    use crate::ai::AiSuggestionService;
    use crate::policy::PolicyConfig;
    use crate::state::AppState;

    use super::build_service_description;

    #[test]
    fn service_description_mentions_core_endpoints() {
        let store = StoreService::new(StoreConfig::default()).expect("store should initialize");
        let reasoner = ReasonerService::new(ReasonerConfig::default());
        let state = AppState::new(
            store,
            reasoner,
            PolicyConfig::default(),
            AiSuggestionService::disabled(),
        );
        let ttl = build_service_description(&state);

        assert!(ttl.contains("/dataset/query"));
        assert!(ttl.contains("/dataset/update"));
        assert!(ttl.contains("/dataset/tell"));
        assert!(ttl.contains("/dataset/data"));
        assert!(ttl.contains("/metrics"));
        assert!(ttl.contains("/ops"));
    }

    #[test]
    fn service_description_omits_disabled_optional_endpoints() {
        let store = StoreService::new(StoreConfig::default()).expect("store should initialize");
        let reasoner = ReasonerService::new(ReasonerConfig::default());
        let state = AppState::new(
            store,
            reasoner,
            PolicyConfig {
                expose_operator_ui: false,
                expose_metrics: false,
                ..PolicyConfig::default()
            },
            AiSuggestionService::disabled(),
        );
        let ttl = build_service_description(&state);

        assert!(!ttl.contains("nrese:metricsEndpoint"));
        assert!(!ttl.contains("nrese:operatorEndpoint"));
        assert!(ttl.contains("nrese:graphStoreEnabled \"true\""));
    }
}
