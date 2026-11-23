// SPDX-License-Identifier: CC0-1.0

//! The PERFORMANCE LOG EVENTS a cluster Container Insights is on for publishes, beside the
//! metric series it publishes into CloudWatch ([`super::container_insights`]).
//!
//! # The rule this module implements
//!
//! Container Insights collects a cluster's performance as LOG EVENTS as well as as metrics:
//! for a cluster the setting is on for, one performance log group is kept per cluster
//! ([`performance_log_group`]) and each collection writes one document per thing it collected
//! for — the cluster itself, each service of it and the tasks of each task definition family
//! running in it — carrying the figures that collection took. A cluster the setting is OFF for
//! collects nothing, so it has no such group and no such event: the group appearing is the
//! observable fact that the collection is running, and it is what a consumer subscribes to,
//! attaches a retention policy to or queries.
//!
//! The documents are the collection's own figures, named as the metrics they were published
//! under, beside the identity of what they were collected for: a cluster document carries the
//! cluster's name, a service document the service's name beside it and a task document the
//! task definition family's, which is how a reader of the group tells the three apart.
//!
//! # Where they are written
//!
//! Into CloudWatch Logs' own ingest path, through the ONE vended-logs write this workspace
//! holds ([`::service_common::cloudwatch_logs::delivered_stamped_over`]): the group and the
//! streams are created as the first collection is written, and each document is one log event
//! stamped with the instant the collection was taken, so a consumer reads it in the period it
//! belongs to. What a client then reads back with `GetLogEvents`, `FilterLogEvents` or a
//! Logs Insights query is what this collection wrote, and every consumer the group is
//! configured with — its metric filters, its subscription filters, its data protection
//! policy — sees these events exactly as it sees a client's own.
//!
//! Which STREAM a document goes to is WHOSE telemetry it is: the cluster's own collection
//! travels the cluster's telemetry stream ([`cluster_telemetry_stream`]), and one task's own
//! collection the stream of that task — [`fargate_telemetry_stream`] for a task Fargate runs,
//! which holds no container instance of its own, and otherwise the id of the container
//! INSTANCE the task runs on, which is the stream that instance's agent writes. A task this
//! store holds no host for travels with the cluster's own collection rather than through a
//! stream named after nothing.
//!
//! The documents carry their figures' declaration at the top level
//! ([`METRIC_DIRECTIVE_MEMBER`]) rather than under an Embedded Metric Format envelope, which
//! is the one thing to know about where their series come from: the series of a collection are
//! published into the metric store by the collection itself
//! ([`super::container_insights`]) — they have to be, so that a cluster's series exists, and
//! its alarms are evaluated, on a deployment holding no log store at all — so a document is a
//! REPORT of those series rather than a second write of them, and ingesting one publishes
//! nothing twice.
//!
//! Delivery is best effort, as every vended-logs write is: a deployment with no CloudWatch
//! Logs mounted, or a group a client has since deleted, loses the events, and the metric
//! series the same collection published is not failed for it.

use ::std::collections::BTreeMap;
use ::std::time::{SystemTime, UNIX_EPOCH};

/// The log group Container Insights keeps one cluster's performance log events in, by the
/// naming it uses for them: `/aws/ecs/containerinsights/<cluster-name>/performance`.
#[must_use]
pub fn performance_log_group(cluster_name: &str) -> String {
    format!("{PERFORMANCE_GROUP_PREFIX}{cluster_name}{PERFORMANCE_GROUP_SUFFIX}")
}

/// What a performance log group's name begins with, and what it ends with
/// ([`performance_log_group`]).
pub const PERFORMANCE_GROUP_PREFIX: &str = "/aws/ecs/containerinsights/";
pub const PERFORMANCE_GROUP_SUFFIX: &str = "/performance";

/// The `Type` member a document collected for a whole CLUSTER carries, the one a document
/// collected for one SERVICE of it carries, and the one a document collected for the TASKS of
/// one task definition family carries — which is how a reader of the group tells what a
/// document is about.
pub const CLUSTER_DOCUMENT_TYPE: &str = "Cluster";
pub const SERVICE_DOCUMENT_TYPE: &str = "Service";
pub const TASK_DOCUMENT_TYPE: &str = "Task";

/// The member every document carries its kind under, the one it carries the instant it was
/// collected at under, and the version of the document layout itself — the three members a
/// performance log event carries beside the figures and the identity of what they are of.
const TYPE_MEMBER: &str = "Type";
const TIMESTAMP_MEMBER: &str = "Timestamp";
const VERSION_MEMBER: &str = "Version";

/// The layout version these documents are written in, which is the one Container Insights
/// performance log events carry.
const DOCUMENT_VERSION: &str = "0";

/// What ONE cluster's collection writes: the cluster it was taken for, and the documents it
/// produced, grouped by the stream each belongs in ([`PerformanceStream`]).
#[derive(Debug, Clone, Default)]
pub struct ClusterPerformance {
    /// The cluster the collection was taken for, which is the group it is written to
    /// ([`performance_log_group`]).
    pub cluster_name: String,
    /// The streams of that group this collection writes into.
    pub streams: Vec<PerformanceStream>,
}

/// The documents of ONE stream of a cluster's performance log group.
#[derive(Debug, Clone)]
pub struct PerformanceStream {
    /// The stream they are written in (see this module's header).
    pub log_stream: String,
    /// The documents, each stamped with the instant the collection was taken, in milliseconds
    /// since the epoch — the pairs the vended-logs write takes.
    pub documents: Vec<(i64, String)>,
}

/// The stream a CLUSTER's own collection travels: its cluster-level document and the
/// service-level ones beside it.
#[must_use]
pub fn cluster_telemetry_stream(cluster_name: &str) -> String {
    format!("ClusterTelemetry-{cluster_name}")
}

/// The stream the collection of ONE task Fargate runs travels, named after that task.
#[must_use]
pub fn fargate_telemetry_stream(task_id: &str) -> String {
    format!("FargateTelemetry-{task_id}")
}

/// ONE document: `kind` collected for the thing `identity` names, carrying `figures` by the
/// names their metrics were published under, declared over `dimension_sets`, as of `at`.
///
/// The figures are the collection's OWN numbers, so a document says exactly what the series
/// beside it says, and the directive it declares them under ([`METRIC_DIRECTIVE_MEMBER`])
/// names the dimension sets those figures are published under and the unit each is measured in
/// — what a consumer of the group reads a figure's identity and unit off.
#[must_use]
pub fn performance_document(
    kind: &str,
    identity: &BTreeMap<String, String>,
    figures: &[(&str, f64, &str)],
    dimension_sets: &[&[&str]],
    at: SystemTime,
) -> String {
    let mut document = ::serde_json::Map::new();
    document.insert(
        VERSION_MEMBER.to_string(),
        ::serde_json::Value::String(DOCUMENT_VERSION.to_string()),
    );
    document.insert(
        TYPE_MEMBER.to_string(),
        ::serde_json::Value::String(kind.to_string()),
    );
    document.insert(
        TIMESTAMP_MEMBER.to_string(),
        ::serde_json::Value::from(epoch_millis(at)),
    );
    for (member, value) in identity {
        document.insert(
            member.clone(),
            ::serde_json::Value::String(value.clone()),
        );
    }
    for (metric_name, figure, _unit) in figures {
        if let Some(number) = ::serde_json::Number::from_f64(*figure) {
            document.insert((*metric_name).to_string(), ::serde_json::Value::Number(number));
        }
    }
    document.insert(
        METRIC_DIRECTIVE_MEMBER.to_string(),
        ::serde_json::Value::Array(vec![metric_directive(figures, dimension_sets)]),
    );
    ::serde_json::Value::Object(document).to_string()
}

/// The member a document declares its figures under, and the three members of one declaration:
/// the namespace they are published in, the dimension sets they are published under and the
/// figures themselves, each by name and unit.
pub const METRIC_DIRECTIVE_MEMBER: &str = "CloudWatchMetrics";
const NAMESPACE_MEMBER: &str = "Namespace";
const DIMENSIONS_MEMBER: &str = "Dimensions";
const METRICS_MEMBER: &str = "Metrics";
const NAME_MEMBER: &str = "Name";
const UNIT_MEMBER: &str = "Unit";

/// ONE declaration of `figures` over `dimension_sets`, in the Container Insights namespace.
fn metric_directive(
    figures: &[(&str, f64, &str)],
    dimension_sets: &[&[&str]],
) -> ::serde_json::Value {
    let dimensions: Vec<::serde_json::Value> = dimension_sets
        .iter()
        .map(|set| {
            ::serde_json::Value::Array(
                set.iter()
                    .map(|name| ::serde_json::Value::String((*name).to_string()))
                    .collect(),
            )
        })
        .collect();
    let metrics: Vec<::serde_json::Value> = figures
        .iter()
        .map(|(metric_name, _figure, unit)| {
            ::serde_json::json!({
                NAME_MEMBER: metric_name,
                UNIT_MEMBER: unit,
            })
        })
        .collect();
    ::serde_json::json!({
        NAMESPACE_MEMBER: super::container_insights::CONTAINER_INSIGHTS_NAMESPACE,
        DIMENSIONS_MEMBER: dimensions,
        METRICS_MEMBER: metrics,
    })
}

/// `at` in milliseconds since the Unix epoch, which is what a log event's timestamp is
/// expressed in (`InputLogEvent$timestamp`). An instant before the epoch, which no clock
/// reports, reads as the epoch itself.
#[must_use]
pub fn epoch_millis(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// Write the performance log events of every collection in `collected`, each into its own
/// cluster's performance log group.
///
/// Called with NO guard of this service's store held: it awaits the write into CloudWatch
/// Logs' own ingest path, which is the discipline every cross-service write in this crate
/// follows.
pub async fn deliver_performance_logs(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    collected: &[ClusterPerformance],
) {
    for collection in collected {
        let log_group = performance_log_group(&collection.cluster_name);
        for stream in &collection.streams {
            if stream.documents.is_empty() {
                continue;
            }
            let delivered = ::service_common::cloudwatch_logs::delivered_stamped_over(
                callback,
                region,
                &log_group,
                &stream.log_stream,
                &stream.documents,
            )
            .await;
            if !delivered {
                ::tracing::warn!(
                    log_group = %log_group,
                    log_stream = %stream.log_stream,
                    "this cluster's Container Insights performance log events could not be \
                     delivered"
                );
            }
        }
    }
}

/// The member a task document carries its own task's id under, beside the cluster's name and
/// the family of the definition it runs.
pub const TASK_ID_MEMBER: &str = "TaskId";

/// The members a TASK document carries beside those: the revision of the definition it runs
/// (`TaskDefinition$revision`), the launch type it was placed under (`Task$launchType`) and the
/// status it is in (`Task$lastStatus`) — what identifies the task a collection was taken of.
pub const REVISION_MEMBER: &str = "TaskDefinitionRevision";
pub const LAUNCH_TYPE_MEMBER: &str = "LaunchType";
pub const KNOWN_STATUS_MEMBER: &str = "KnownStatus";
