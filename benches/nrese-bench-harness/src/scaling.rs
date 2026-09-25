//! `write-scaling`: how write latency and load throughput behave as the dataset grows.
//!
//! The dataset is grown in steps with synthetic, history-shaped entities (type, language-
//! tagged label, a link, an `xsd:date`). After each step the harness measures the latency of
//! single-triple `INSERT DATA` requests and of a `COUNT` query. A store whose write cost
//! follows the change (engine v2 target) shows flat insert latency; a store that copies the
//! dataset per write (engine v1) shows linear growth. Roadmap gate P1 is judged on this.

use std::time::Instant;

use anyhow::{Result, bail};
use reqwest::Client;
use serde::Serialize;

use crate::compat_common::{
    GraphWriteRequest, RequestExecutionOptions, execute_graph_write_raw, execute_query_raw,
    execute_update_raw, require_success_http,
};
use crate::io::write_json_report;
use crate::layout::ServiceTarget;
use crate::model::{CompatGraphTarget, CompatHeaders, WriteScalingConfig};
use crate::normalize::percentile;

const TRIPLES_PER_ENTITY: u64 = 4;
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const XSD_DATE: &str = "http://www.w3.org/2001/XMLSchema#date";

#[derive(Debug, Serialize)]
pub struct WriteScalingReport {
    pub mode: &'static str,
    pub samples_per_step: usize,
    pub services: Vec<ServiceScalingReport>,
}

#[derive(Debug, Serialize)]
pub struct ServiceScalingReport {
    pub label: &'static str,
    pub base_url: String,
    pub steps: Vec<ScalingStep>,
}

#[derive(Debug, Serialize)]
pub struct ScalingStep {
    pub triples: u64,
    pub load_ms: u128,
    pub load_triples_per_sec: u64,
    pub insert_p50_ms: u128,
    pub insert_max_ms: u128,
    pub count_query_ms: u128,
}

pub async fn run_write_scaling(config: WriteScalingConfig) -> Result<()> {
    if config.steps.windows(2).any(|pair| pair[0] >= pair[1]) || config.steps.is_empty() {
        bail!("--steps must be a non-empty, strictly increasing list of triple counts");
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()?;

    let mut targets = vec![ServiceTarget::nrese(config.nrese.clone())];
    if let Some(reference) = &config.reference {
        targets.push(ServiceTarget::reference(
            reference.kind,
            reference.connection.clone(),
        ));
    }

    let mut services = Vec::new();
    for target in &targets {
        println!(
            "== write-scaling: {} ({}) ==",
            target.label, target.base_url
        );
        services.push(scale_target(&client, target, &config).await?);
    }

    if let Some(path) = &config.report_json_path {
        write_json_report(
            path.clone(),
            &WriteScalingReport {
                mode: "write-scaling",
                samples_per_step: config.samples,
                services,
            },
        )?;
    }
    Ok(())
}

async fn scale_target(
    client: &Client,
    target: &ServiceTarget,
    config: &WriteScalingConfig,
) -> Result<ServiceScalingReport> {
    if config.reset {
        update(client, target, "DROP ALL").await?;
    }

    let chunk_entities = (config.chunk_triples / TRIPLES_PER_ENTITY).max(1);
    let mut loaded_entities = 0u64;
    let mut steps = Vec::with_capacity(config.steps.len());
    for &step_triples in &config.steps {
        let target_entities = step_triples / TRIPLES_PER_ENTITY;
        let load_started = Instant::now();
        while loaded_entities < target_entities {
            let count = chunk_entities.min(target_entities - loaded_entities);
            load(
                client,
                target,
                &entities(loaded_entities, count, target_entities),
            )
            .await?;
            loaded_entities += count;
        }
        let load_ms = load_started.elapsed().as_millis();
        let loaded_triples =
            step_triples.saturating_sub(steps.last().map_or(0, |s: &ScalingStep| s.triples));

        let mut latencies = Vec::with_capacity(config.samples);
        for sample in 0..config.samples {
            let started = Instant::now();
            update(
                client,
                target,
                &format!(
                    "INSERT DATA {{ <http://example.org/probe/{step_triples}/{sample}> <http://example.org/knows> <http://example.org/person/0> }}"
                ),
            )
            .await?;
            latencies.push(started.elapsed().as_millis());
        }
        latencies.sort_unstable();

        let started = Instant::now();
        let outcome = execute_query_raw(
            client,
            target,
            "SELECT (COUNT(*) AS ?c) WHERE { ?s a <http://example.org/Person> }",
            "application/sparql-results+json",
            &CompatHeaders::new(),
            RequestExecutionOptions::default(),
        )
        .await?;
        require_success_http(target, "query", &outcome)?;
        let count_query_ms = started.elapsed().as_millis();

        let step = ScalingStep {
            triples: step_triples,
            load_ms,
            load_triples_per_sec: (loaded_triples as u128 * 1000 / load_ms.max(1)) as u64,
            insert_p50_ms: percentile(&latencies, 50),
            insert_max_ms: latencies.last().copied().unwrap_or_default(),
            count_query_ms,
        };
        println!(
            "triples={:>11} load={:>7}ms ({:>8} t/s) insert p50={:>6}ms max={:>6}ms count={:>6}ms",
            step.triples,
            step.load_ms,
            step.load_triples_per_sec,
            step.insert_p50_ms,
            step.insert_max_ms,
            step.count_query_ms
        );
        steps.push(step);
    }

    Ok(ServiceScalingReport {
        label: target.label,
        base_url: target.base_url.clone(),
        steps,
    })
}

async fn update(client: &Client, target: &ServiceTarget, update: &str) -> Result<()> {
    let outcome = execute_update_raw(
        client,
        target,
        update,
        &CompatHeaders::new(),
        RequestExecutionOptions::default(),
    )
    .await?;
    require_success_http(target, "update", &outcome)?;
    Ok(())
}

async fn load(client: &Client, target: &ServiceTarget, payload: &[u8]) -> Result<()> {
    let outcome = execute_graph_write_raw(
        client,
        target,
        GraphWriteRequest {
            graph_target: &CompatGraphTarget::DefaultGraph,
            content_type: "application/n-triples",
            payload,
            replace: false,
            extra_headers: &CompatHeaders::new(),
            options: RequestExecutionOptions::default(),
        },
    )
    .await?;
    require_success_http(target, "graph write", &outcome)?;
    Ok(())
}

/// N-Triples for entities `[start, start + count)`; links point into `[0, universe)`.
fn entities(start: u64, count: u64, universe: u64) -> Vec<u8> {
    let mut out = String::with_capacity((count * 4 * 90) as usize);
    for i in start..start + count {
        let subject = format!("<http://example.org/person/{i}>");
        let friend = (i.wrapping_mul(7_919) + 1) % universe.max(1);
        out.push_str(&format!(
            "{subject} <{RDF_TYPE}> <http://example.org/Person> .\n"
        ));
        out.push_str(&format!("{subject} <{RDFS_LABEL}> \"Person {i}\"@de .\n"));
        out.push_str(&format!(
            "{subject} <http://example.org/knows> <http://example.org/person/{friend}> .\n"
        ));
        out.push_str(&format!(
            "{subject} <http://example.org/born> \"14{:02}-01-01\"^^<{XSD_DATE}> .\n",
            i % 100
        ));
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::{TRIPLES_PER_ENTITY, entities};

    #[test]
    fn generator_emits_four_triples_per_entity() {
        let payload = String::from_utf8(entities(10, 3, 100)).expect("utf8");
        assert_eq!(payload.lines().count() as u64, 3 * TRIPLES_PER_ENTITY);
        assert!(payload.contains("<http://example.org/person/12>"));
    }
}
