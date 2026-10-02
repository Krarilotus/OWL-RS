use axum::http::HeaderMap;

use crate::access::AccessView;
use crate::auth::Identity;
use crate::error::ApiError;
use crate::policy::PolicyAction;
use crate::state::AppState;

pub async fn enforce(
    state: &AppState,
    action: PolicyAction,
    headers: &HeaderMap,
) -> Result<Identity, ApiError> {
    state.enforce_policy_action(action, headers).await
}

pub async fn enforce_operator_read(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Identity, ApiError> {
    if !state.runtime_posture().operator_surface_enabled {
        return Err(ApiError::not_found("operator UI is disabled by policy"));
    }
    enforce(state, PolicyAction::OperatorRead, headers).await
}

pub async fn enforce_query_read(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Identity, ApiError> {
    enforce(state, PolicyAction::QueryRead, headers).await
}

pub async fn enforce_update_write(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Identity, ApiError> {
    if !state.runtime_posture().sparql_update_enabled {
        return Err(ApiError::not_found(
            "SPARQL update endpoint is disabled by deployment posture",
        ));
    }
    enforce(state, PolicyAction::UpdateWrite, headers).await
}

pub async fn enforce_tell_write(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Identity, ApiError> {
    if !state.runtime_posture().tell_enabled {
        return Err(ApiError::not_found(
            "tell endpoint is disabled by deployment posture",
        ));
    }
    enforce(state, PolicyAction::TellWrite, headers).await
}

pub async fn enforce_admin_write(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Identity, ApiError> {
    if !state.runtime_posture().admin_surface_enabled {
        return Err(ApiError::not_found(
            "admin mutation endpoints are disabled by deployment posture",
        ));
    }
    enforce(state, PolicyAction::AdminWrite, headers).await
}

pub async fn enforce_graph_read(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Identity, ApiError> {
    if !state.runtime_posture().graph_store_enabled {
        return Err(ApiError::not_found(
            "graph store read surface is disabled by deployment posture",
        ));
    }
    enforce(state, PolicyAction::GraphRead, headers).await
}

pub async fn enforce_graph_write(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Identity, ApiError> {
    if !state.runtime_posture().graph_write_enabled {
        return Err(ApiError::not_found(
            "graph store write surface is disabled by deployment posture",
        ));
    }
    enforce(state, PolicyAction::GraphWrite, headers).await
}

pub async fn enforce_service_description_read(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Identity, ApiError> {
    enforce(state, PolicyAction::ServiceDescriptionRead, headers).await
}

pub async fn enforce_metrics_read(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Identity, ApiError> {
    if !state.runtime_posture().metrics_enabled {
        return Err(ApiError::not_found(
            "metrics endpoint is disabled by policy",
        ));
    }
    enforce(state, PolicyAction::MetricsRead, headers).await
}

/// What `identity` may read and write (graph-level access control, [`crate::access`]).
pub fn view(state: &AppState, identity: &Identity) -> AccessView {
    state.policy().access_view(identity)
}

/// [`enforce_query_read`], and what the requester may read.
pub async fn query_access(state: &AppState, headers: &HeaderMap) -> Result<AccessView, ApiError> {
    let identity = enforce_query_read(state, headers).await?;
    Ok(view(state, &identity))
}

/// [`enforce_update_write`], and what the requester may read and write.
pub async fn update_access(state: &AppState, headers: &HeaderMap) -> Result<AccessView, ApiError> {
    let identity = enforce_update_write(state, headers).await?;
    Ok(view(state, &identity))
}

/// [`enforce_graph_read`], and what the requester may read.
pub async fn graph_read_access(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AccessView, ApiError> {
    let identity = enforce_graph_read(state, headers).await?;
    Ok(view(state, &identity))
}

/// [`enforce_graph_write`], and what the requester may write.
pub async fn graph_write_access(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AccessView, ApiError> {
    let identity = enforce_graph_write(state, headers).await?;
    Ok(view(state, &identity))
}

/// [`enforce_tell_write`], and what the requester may write.
pub async fn tell_access(state: &AppState, headers: &HeaderMap) -> Result<AccessView, ApiError> {
    let identity = enforce_tell_write(state, headers).await?;
    Ok(view(state, &identity))
}

/// [`enforce_query_read`] for endpoints that read the whole dataset (autocomplete,
/// classification, SHACL validation, query suggestions): only for requesters who may read
/// every graph.
pub async fn enforce_whole_read(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if query_access(state, headers).await?.reads_everything() {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "this endpoint reads every graph, and the requester may read only some",
        ))
    }
}

/// Fails unless the requester may write `graph`.
pub fn check_writable(view: &AccessView, graph: &nrese_rdf::GraphName) -> Result<(), ApiError> {
    if view.can_write(graph) {
        Ok(())
    } else {
        Err(ApiError::forbidden(format!(
            "the requester may not write {}",
            match graph {
                nrese_rdf::GraphName::DefaultGraph => "the default graph".to_owned(),
                graph => graph.to_string(),
            }
        )))
    }
}

/// The graph a Graph Store Protocol target names.
pub fn target_graph(target: &nrese_store::GraphTarget) -> Result<nrese_rdf::GraphName, ApiError> {
    Ok(match target {
        nrese_store::GraphTarget::DefaultGraph => nrese_rdf::GraphName::DefaultGraph,
        nrese_store::GraphTarget::NamedGraph(iri) => nrese_rdf::NamedNode::new(iri.as_str())
            .map_err(|error| ApiError::bad_request(error.to_string()))?
            .into(),
    })
}
