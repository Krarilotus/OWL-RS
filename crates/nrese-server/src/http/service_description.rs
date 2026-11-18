use crate::repositories::DEFAULT_REPOSITORY;
use crate::runtime_posture::{
    GRAPH_STORE_ENDPOINT, METRICS_ENDPOINT, OPERATOR_UI_PATH, QUERY_ENDPOINT,
    SERVICE_DESCRIPTION_ENDPOINT, SHACL_ENDPOINT, SPARQL_ENDPOINT, TELL_ENDPOINT, UPDATE_ENDPOINT,
};
use crate::state::AppState;

/// The endpoints of the repository a description describes: the default repository's
/// short aliases (`/dataset/…`), another's under `/api/v1/repositories/{id}`.
struct Endpoints {
    query: String,
    update: String,
    sparql: String,
    tell: String,
    graph_store: String,
    shacl: String,
    description: String,
}

impl Endpoints {
    fn of(repository: &str) -> Self {
        if repository == DEFAULT_REPOSITORY {
            return Self {
                query: QUERY_ENDPOINT.to_owned(),
                update: UPDATE_ENDPOINT.to_owned(),
                sparql: SPARQL_ENDPOINT.to_owned(),
                tell: TELL_ENDPOINT.to_owned(),
                graph_store: GRAPH_STORE_ENDPOINT.to_owned(),
                shacl: SHACL_ENDPOINT.to_owned(),
                description: SERVICE_DESCRIPTION_ENDPOINT.to_owned(),
            };
        }
        let at = |path: &str| format!("/api/v1/repositories/{repository}/{path}");
        Self {
            query: at("query"),
            update: at("update"),
            sparql: at("sparql"),
            tell: at("tell"),
            graph_store: at("data"),
            shacl: at("shacl"),
            description: at("service-description"),
        }
    }
}

/// The SPARQL service description of `state`'s repository: its endpoints, and what is on
/// as the runtime posture says (the facts the JSON capabilities report).
pub fn build_service_description(state: &AppState) -> String {
    let posture = state.runtime_posture();
    let Endpoints {
        query: query_endpoint,
        update: update_endpoint,
        sparql: sparql_endpoint,
        tell: tell_endpoint,
        graph_store: graph_store_endpoint,
        shacl: shacl_endpoint,
        description: description_endpoint,
    } = Endpoints::of(state.repository_id());
    let reasoning_mode = state.reasoner_mode_name();
    let reasoning_profile = state.reasoner_profile_name();
    let reasoning_read_model = state.reasoner_read_model_name();
    let graph_store_enabled = bool_literal(posture.graph_store_enabled);
    let graph_store_write_enabled = bool_literal(posture.graph_write_enabled);
    let sparql_update_enabled = bool_literal(posture.sparql_update_enabled);
    let tell_enabled = bool_literal(posture.tell_enabled);
    let federated_service_enabled = bool_literal(posture.federated_service_enabled);
    let update_endpoint = posture
        .sparql_update_enabled
        .then_some(format!("   nrese:updateEndpoint <{update_endpoint}> ;\n"))
        .unwrap_or_default();
    let tell_endpoint = posture
        .tell_enabled
        .then_some(format!("   nrese:tellEndpoint <{tell_endpoint}> ;\n"))
        .unwrap_or_default();
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
   sd:endpoint <{query_endpoint}> ;\n\
   sd:supportedLanguage sd:SPARQL11Query ;\n\
   sd:resultFormat format:SPARQL_Results_JSON , format:SPARQL_Results_XML , format:SPARQL_Results_CSV , format:SPARQL_Results_TSV , format:N-Triples , format:Turtle , format:RDF_XML , format:JSON-LD , format:N-Quads , format:TriG ;\n\
   sd:inputFormat format:N-Triples , format:Turtle , format:RDF_XML , format:JSON-LD , format:N-Quads , format:TriG ;\n\
   nrese:sparqlEndpoint <{sparql_endpoint}> ;\n\
   nrese:serviceDescriptionEndpoint <{description_endpoint}> ;\n\
{update_endpoint}\
{tell_endpoint}\
   nrese:graphStoreEndpoint <{graph_store_endpoint}> ;\n\
   nrese:shaclEndpoint <{shacl_endpoint}> ;\n\
{metrics_endpoint}\
{operator_endpoint}\
   nrese:reasoningMode \"{reasoning_mode}\" ;\n\
   nrese:reasoningProfile \"{reasoning_profile}\" ;\n\
   nrese:reasoningReadModel \"{reasoning_read_model}\" ;\n\
   nrese:graphStoreEnabled \"{graph_store_enabled}\" ;\n\
   nrese:graphStoreWriteEnabled \"{graph_store_write_enabled}\" ;\n\
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
            crate::runtime_posture::DeploymentPosture::OpenWorkbench,
        );
        let ttl = build_service_description(&state);

        assert!(ttl.contains("/dataset/query"));
        assert!(ttl.contains("nrese:sparqlEndpoint </dataset/sparql>"));
        assert!(ttl.contains("format:JSON-LD"));
        assert!(ttl.contains("/dataset/update"));
        assert!(ttl.contains("/dataset/tell"));
        assert!(ttl.contains("/dataset/data"));
        assert!(ttl.contains("/metrics"));
        assert!(ttl.contains("/ops"));
        assert!(ttl.contains("nrese:reasoningReadModel \"asserted-only\""));
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
            crate::runtime_posture::DeploymentPosture::OpenWorkbench,
        );
        let ttl = build_service_description(&state);

        assert!(!ttl.contains("nrese:metricsEndpoint"));
        assert!(!ttl.contains("nrese:operatorEndpoint"));
        assert!(ttl.contains("nrese:graphStoreEnabled \"true\""));
    }
}
