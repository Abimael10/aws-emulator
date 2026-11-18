// SPDX-License-Identifier: CC0-1.0

//! The data plane's NODES APIs: `GET /_nodes[/{node}]` and the node statistics
//! `GET /_nodes/stats[/{metric}]`, for the one statistics section this cluster publishes
//! ([`PUBLISHED_METRIC`]).
//!
//! Both report the CENSUS `DescribeDomainNodes` reports — one node per node the stored
//! `ClusterConfig` provisions ([`crate::ops::describe_domain_nodes::domain_nodes`]) — so the
//! control plane and the cluster can never describe one domain's topology differently.
//!
//! What a managed domain publishes of a node is the configuration the service provisioned it
//! with: its roles, the engine release it runs, the Availability Zone it sits in, the
//! processors the instance type it runs on has, and the size of the volume attached to it.
//! The cluster's own addressing and process detail — a node's host, its IP, its transport
//! address, its settings, the JVM it runs, and the operating system's name and version — is
//! service-internal and is published in NONE of them: a section that carries both is reported
//! with the members the service publishes and without the ones it withholds, rather than left
//! out altogether or invented.

use super::*;

/// The nodes of this domain's cluster, as `GET /_nodes` reports them.
///
/// `node` is the id a request addressing ONE node names; a filter matching no node of the
/// cluster reports no node, which is what the engine answers a node filter that selects
/// nothing.
pub(super) fn nodes_info(domain: &Domain, node: Option<&str>) -> Value {
    let (_, version) = engine_distribution(&domain.engine_version);
    reported_nodes(domain, node, |reported| {
        let processors = available_processors(reported.instance_type.as_deref());
        let mut sections = Map::new();
        sections.insert("version".to_string(), json!(version));
        // The operating system the node runs on, as a managed domain publishes it: the
        // processors of the instance type it was provisioned on — which is what a client
        // sizes its own work from — and nothing of that system's own name or version.
        sections.insert(
            "os".to_string(),
            json!({
                "available_processors": processors,
                "allocated_processors": processors,
            }),
        );
        sections
    })
}

/// The node statistics `GET /_nodes/stats` reports, over the same census.
///
/// ONE statistics section is published ([`PUBLISHED_METRIC`]), so this is the whole report
/// either addressing of it answers; a path naming another metric is not served at all
/// ([`publishes_metric`]), rather than answered with a report that carries no section for
/// what it asked about.
pub(super) fn nodes_stats(domain: &Domain, node: Option<&str>) -> Value {
    reported_nodes(domain, node, |reported| {
        let mut sections = Map::new();
        // The file system the node's data sits on: the volume the service attached to it,
        // whose SIZE is the domain's own `EBSOptions`, with the path it is mounted at — a
        // host path of the service's own — published nowhere. A node whose storage size the
        // record does not name reports no file system rather than a size nothing states.
        if let Some(total) = volume_bytes(reported.storage_size.as_deref()) {
            sections.insert(
                "fs".to_string(),
                json!({ "total": { "total_in_bytes": total } }),
            );
        }
        sections
    })
}

/// Whether `metric` names statistics this cluster publishes: the one section it holds, and
/// the name that asks for every section there is.
pub(super) fn publishes_metric(metric: &str) -> bool {
    metric
        .split(',')
        .all(|named| named == PUBLISHED_METRIC || named == METRIC_ALL)
}

/// The whole report both APIs answer: the `_nodes` summary, the cluster's name and one entry
/// per node of the census, each carrying the node's own identity and whatever `sections` adds
/// to it.
fn reported_nodes(
    domain: &Domain,
    node: Option<&str>,
    sections: impl Fn(&crate::ops::types::DomainNodesStatus) -> Map<String, Value>,
) -> Value {
    let dedicated_master = domain.cluster_config.dedicated_master_enabled == Some(true);
    let mut nodes = Map::new();
    for reported in crate::ops::describe_domain_nodes::domain_nodes(domain) {
        let Some(id) = reported.node_id.clone() else {
            continue;
        };
        if node.is_some_and(|named| named != id) {
            continue;
        }
        let mut attributes = Map::new();
        if let Some(zone) = &reported.availability_zone {
            attributes.insert("availability_zone".to_string(), json!(zone));
        }
        let mut entry = Map::new();
        entry.insert("name".to_string(), json!(id));
        entry.insert(
            "roles".to_string(),
            json!(node_roles(
                reported.node_type.as_deref().unwrap_or_default(),
                dedicated_master
            )),
        );
        entry.insert("attributes".to_string(), Value::Object(attributes));
        entry.extend(sections(&reported));
        nodes.insert(id, Value::Object(entry));
    }
    let total = nodes.len();
    json!({
        "_nodes": { "total": total, "successful": total, "failed": 0 },
        "cluster_name": cluster_name(domain),
        "nodes": Value::Object(nodes),
    })
}

/// The metric name that asks for every section of the node statistics.
const METRIC_ALL: &str = "_all";

/// The one statistics section this cluster publishes: the node's file system, whose figures
/// come from the volume the service attached to it.
const PUBLISHED_METRIC: &str = "fs";

/// How many bytes a volume of one gibibyte holds — `EBSOptions$VolumeSize` is "the size (in
/// GiB) of the EBS volume", and the engine reports a file system in bytes.
const BYTES_PER_GIB: i64 = 1024 * 1024 * 1024;

/// The bytes the volume attached to a node holds, from the storage size the stored record
/// names for it, and `None` where it names none.
fn volume_bytes(storage_size: Option<&str>) -> Option<i64> {
    let size: i64 = storage_size?.parse().ok()?;
    (size > 0).then(|| size.saturating_mul(BYTES_PER_GIB))
}

/// How many processors a node provisioned on `instance_type` runs on.
///
/// Read off the SIZE scale this crate already expresses a per-size quantity against
/// ([`crate::state::size_normalization_factor`], AWS's published instance-size normalization
/// factors): from `large` upward the service's supported-instance-types table publishes one
/// vCPU per [`UNITS_PER_PROCESSOR`] of those units (a `large` is four units and has two vCPUs,
/// an `xlarge` eight and four, a `12xlarge` ninety-six and forty-eight), and every size below
/// `large` publishes [`PROCESSORS_FLOOR`] — which is also what a node whose instance type the
/// record does not name reports, since a node still runs on processors.
fn available_processors(instance_type: Option<&str>) -> i64 {
    let Some(factor) = instance_type.and_then(state::size_normalization_factor) else {
        return PROCESSORS_FLOOR;
    };
    #[allow(clippy::cast_possible_truncation)]
    let processors = (factor / UNITS_PER_PROCESSOR).trunc() as i64;
    processors.max(PROCESSORS_FLOOR)
}

/// How many normalization units of its instance class one processor of it is — see
/// [`available_processors`].
const UNITS_PER_PROCESSOR: f64 = 2.0;

/// The fewest processors any instance this service is offered with runs on, which is what
/// every size below `large` publishes — see [`available_processors`].
const PROCESSORS_FLOOR: i64 = 2;

/// The roles OpenSearch publishes for a node of this domain, from the ROLE its cluster
/// configuration provisions the node in.
///
/// A data node holds the data and ingest roles, and is cluster-manager eligible as well on a
/// domain that provisions no dedicated master — there has to be somewhere for the cluster
/// manager to be elected. A dedicated master node holds the cluster-manager role alone,
/// which is what makes it dedicated, and a warm node holds the warm role beside the data one
/// it serves searches from. A node type this emulator does not provision holds no role.
pub(super) fn node_roles(node_type: &str, dedicated_master: bool) -> Vec<&'static str> {
    match node_type {
        state::NODE_TYPE_DATA if dedicated_master => vec!["data", "ingest"],
        state::NODE_TYPE_DATA => vec!["cluster_manager", "data", "ingest"],
        state::NODE_TYPE_MASTER => vec!["cluster_manager"],
        state::NODE_TYPE_ULTRAWARM => vec!["data", "warm"],
        _ => Vec::new(),
    }
}
