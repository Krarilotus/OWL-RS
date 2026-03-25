use crate::state::AppState;

pub fn build_service_description(state: &AppState) -> String {
    let reasoning_mode = state.reasoner_mode_name();
    let reasoning_profile = state.reasoner_profile_name();
    let graph_store_enabled = "true";
    let sparql_update_enabled = "true";
    let tell_enabled = "true";
    let federated_service_enabled = "false";

    format!(
        "@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .\n\
@prefix format: <http://www.w3.org/ns/formats/> .\n\
@prefix void: <http://rdfs.org/ns/void#> .\n\
@prefix nrese: <https://nrese.dev/ns/service#> .\n\
\n\
[] a sd:Service ;\n\
   sd:endpoint </dataset/query> ;\n\
   sd:supportedLanguage sd:SPARQL11Query ;\n\
   sd:resultFormat format:SPARQL_Results_JSON , format:SPARQL_Results_XML , format:SPARQL_Results_CSV , format:SPARQL_Results_TSV ;\n\
   nrese:updateEndpoint </dataset/update> ;\n\
   nrese:tellEndpoint </dataset/tell> ;\n\
   nrese:graphStoreEndpoint </dataset/data> ;\n\
   nrese:metricsEndpoint </metrics> ;\n\
   nrese:operatorEndpoint </ops> ;\n\
   nrese:reasoningMode \"{reasoning_mode}\" ;\n\
   nrese:reasoningProfile \"{reasoning_profile}\" ;\n\
   nrese:graphStoreEnabled \"{graph_store_enabled}\" ;\n\
   nrese:sparqlUpdateEnabled \"{sparql_update_enabled}\" ;\n\
   nrese:tellEnabled \"{tell_enabled}\" ;\n\
   nrese:federatedServiceEnabled \"{federated_service_enabled}\" .\n"
    )
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
}
