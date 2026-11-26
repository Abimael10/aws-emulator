// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Logs side of a cluster's `logging`: the group a cluster's control
//! plane logs are exported to, the stream each enabled log type exports under, and the
//! export itself.
//!
//! `LogSetup$enabled` is "If a log type is enabled, that log type exports its control
//! plane logs to CloudWatch Logs. If a log type isn't enabled, that log type doesn't
//! export its control plane logs. Each individual log type can be enabled or disabled
//! independently", so a configuration this service STORES is only half of the behaviour:
//! the export is what a client observes, and it is observed in CloudWatch Logs rather
//! than on the cluster. A cluster that reports `api` enabled and exports nothing is a
//! cluster whose setting had no effect.
//!
//! Three things the export is made of, each of them a value Amazon EKS publishes:
//!
//! * the GROUP is `/aws/eks/<cluster name>/cluster` ([`cluster_log_group_name`]), one per
//!   cluster, and the service creates it when the first enabled type needs it — a client
//!   that enables logging never creates it itself;
//! * the STREAM is the control plane COMPONENT whose logs that type carries
//!   ([`control_plane_log_stream_name`]), so each log type exports under its own stream
//!   and a type nobody enabled has none;
//! * nothing is created at all for a configuration that enables NO type
//!   ([`enabled_log_types`] is empty), which is what makes a disabled type observably
//!   different from an enabled one.
//!
//! The export goes out over the in-process callback, the same seam the rest of this
//! service's cross-service work uses (`super::callback_client!`), and is BEST EFFORT: an
//! emulator with no CloudWatch Logs mounted beside this service, or a write CloudWatch
//! Logs refuses, is reported in this service's own tracing rather than failing the
//! cluster call, which asked for a cluster write and has the configuration stored either
//! way. Nothing here deletes a group or a stream: the log records in it belong to the
//! account, and nothing documents Amazon EKS removing a customer's logs when a type is
//! disabled or a cluster is deleted.
//!
//! Because the export is best effort, WHAT OWES IT is the cluster's own stored
//! configuration and not the single call that asked for it
//! ([`ControlPlaneLogExport::owed_by`], [`export_owed_control_plane_logs`]): a type the
//! cluster reports as enabled and whose stream this service has not managed to open yet is
//! still owed, and the next operation that reads that cluster makes the export. That is
//! also the instant Amazon EKS makes it for an asynchronous configuration change — "cluster
//! updates are asynchronous", and the configuration a `logging` update carries is on the
//! cluster only once the clock completes that update — so a client that polls the update to
//! `Successful` and then goes looking for the group finds it, however the call that started
//! the update was answered: one the SDK retried under the same idempotency token (answered
//! from the update already recorded), and one whose own export CloudWatch Logs could not
//! take at the time, both leave the export owed rather than lost.

use super::*;

/// The log group one cluster's control plane logs are exported to:
/// `/aws/eks/<cluster name>/cluster`, the name Amazon EKS publishes for it.
///
/// Built from the cluster's own name, so each cluster of the account has a group of its
/// own and a client that knows the cluster knows the group.
pub fn cluster_log_group_name(cluster_name: &str) -> String {
    format!("/aws/eks/{cluster_name}/cluster")
}

/// The log types `logging` ENABLES, in the order the model declares them
/// ([`crate::ops::types::LOG_TYPE_VALUES`]).
///
/// A type is enabled when ANY entry of `clusterLogging` that enables logging names it —
/// "each individual log type can be enabled or disabled independently" — so a type named
/// twice, or named in both an enabled and a disabled entry, is reported once and a
/// configuration carrying no entry at all enables nothing. This is the ONE reading of
/// which types are on: [`super::reported_logging`] projects the same answer onto the
/// `Logging` a cluster reports.
pub fn enabled_log_types(logging: Option<&Logging>) -> Vec<&'static str> {
    let setups = logging
        .and_then(|logging| logging.cluster_logging.as_deref())
        .unwrap_or_default();
    LOG_TYPE_VALUES
        .iter()
        .copied()
        .filter(|log_type| {
            setups.iter().any(|setup| {
                setup.enabled.unwrap_or(false)
                    && setup.types.as_deref().is_some_and(|types| {
                        types.iter().any(|named| named == log_type)
                    })
            })
        })
        .collect()
}

/// The control plane component whose logs one log type carries, which is the component
/// Amazon EKS names that type's log streams after: the Kubernetes API server for `api`,
/// its audit log for `audit`, the IAM authenticator for `authenticator`, and the
/// controller manager and scheduler for the two types named after them.
///
/// `None` for a value that is not one of the five types `LogType` declares, which no
/// stored configuration carries (the boundary validator refuses an unmodelled member of
/// the enum) and for which there is no component to name a stream after.
fn control_plane_component(log_type: &str) -> Option<&'static str> {
    match log_type {
        "api" => Some("kube-apiserver"),
        "audit" => Some("kube-apiserver-audit"),
        "authenticator" => Some("authenticator"),
        "controllerManager" => Some("kube-controller-manager"),
        "scheduler" => Some("kube-scheduler"),
        _ => None,
    }
}

/// How many hex characters of the cluster's resource id the stream suffix carries.
const LOG_STREAM_SUFFIX_CHARS: usize = 10;

/// The log stream one enabled log type exports under:
/// `<component>-<suffix>` ([`control_plane_component`]).
///
/// The suffix is derived from the cluster's own stable resource id
/// ([`super::derive_cluster_resource_id`]), so two clusters never name the same stream,
/// the name is the same on every export for one cluster — a later export appends to the
/// stream the earlier one opened — and nothing in the request decides it. `resource_id`
/// is expected to be that 32-hex-character id; a shorter value is used as it is, because
/// a stream name is not what a cluster call is refused over.
pub fn control_plane_log_stream_name(log_type: &str, resource_id: &str) -> Option<String> {
    let component = control_plane_component(log_type)?;
    let suffix = resource_id
        .get(..LOG_STREAM_SUFFIX_CHARS)
        .unwrap_or(resource_id)
        .to_ascii_lowercase();
    Some(format!("{component}-{suffix}"))
}

/// What ONE log record delivered to a type's stream says.
///
/// This emulator runs no Kubernetes control plane, so it synthesises none of that plane's
/// traffic: what it delivers is the one thing it knows about the export — that this type
/// of this cluster's control plane logs is being exported, and the versions of the control
/// plane doing the exporting — written as a JSON object whose members are spelled the way
/// this service's own model spells them (`logType`, `clusterName`, `version`,
/// `platformVersion`). A member the cluster does not carry is left out rather than written
/// as `null`.
fn export_record(
    cluster_name: &str,
    log_type: &str,
    component: &str,
    version: Option<&str>,
    platform_version: Option<&str>,
) -> String {
    let mut document = ::serde_json::Map::new();
    document.insert(
        "clusterName".to_string(),
        ::serde_json::Value::String(cluster_name.to_string()),
    );
    document.insert(
        "logType".to_string(),
        ::serde_json::Value::String(log_type.to_string()),
    );
    document.insert(
        "component".to_string(),
        ::serde_json::Value::String(component.to_string()),
    );
    for (member, value) in [("version", version), ("platformVersion", platform_version)] {
        if let Some(value) = value {
            document.insert(
                member.to_string(),
                ::serde_json::Value::String(value.to_string()),
            );
        }
    }
    document.insert(
        "message".to_string(),
        ::serde_json::Value::String(format!(
            "{component} control plane logs of cluster {cluster_name} are exported to this \
             log stream"
        )),
    );
    ::serde_json::Value::Object(document).to_string()
}

/// Everything one export needs off the cluster it is for, read while the store's lock is
/// held so the export itself runs with none.
#[derive(Debug, Clone)]
pub struct ControlPlaneLogExport {
    /// The cluster whose control plane is exporting, which names both the group and the
    /// Region the group is created in.
    pub cluster_name: String,
    /// The cluster's 32-hex-character resource id, which the stream names are built from.
    pub resource_id: String,
    /// The cluster's Kubernetes version, as the record holds it.
    pub version: Option<String>,
    /// The cluster's Amazon EKS platform version, as the record holds it.
    pub platform_version: Option<String>,
    /// The log types this export turns on, in the model's own order.
    pub log_types: Vec<&'static str>,
}

impl ControlPlaneLogExport {
    /// What `logging` asks this cluster to export, or `None` when it enables no type —
    /// the case in which nothing is created at all.
    pub fn of(record: &ClusterRecord, logging: Option<&Logging>) -> Option<Self> {
        Self::for_types(record, enabled_log_types(logging))
    }

    /// What this cluster still OWES CloudWatch Logs: the types its STORED configuration
    /// enables and whose export this service has not made yet
    /// ([`ClusterRecord::exported_log_types`]), or `None` when it owes nothing.
    ///
    /// This is the reading that makes the export an effect of the cluster's configuration
    /// rather than of one call (see this module's header): a type enabled by an update the
    /// clock has completed, and one whose earlier export did not land, are both owed here.
    pub fn owed_by(record: &ClusterRecord) -> Option<Self> {
        let owed: Vec<&'static str> = enabled_log_types(record.logging.as_ref())
            .into_iter()
            .filter(|log_type| !record.has_exported_log_type(log_type))
            .collect();
        Self::for_types(record, owed)
    }

    /// The export of `log_types` for this cluster, or `None` when the list is empty —
    /// nothing is created for a cluster that exports no type.
    fn for_types(record: &ClusterRecord, log_types: Vec<&'static str>) -> Option<Self> {
        if log_types.is_empty() {
            return None;
        }
        Some(Self {
            cluster_name: record.name.clone(),
            resource_id: derive_cluster_resource_id(&record.arn),
            version: record.version.clone(),
            platform_version: record.platform_version.clone(),
            log_types,
        })
    }
}

/// Make the exports the clusters of `region` still OWE CloudWatch Logs
/// ([`ControlPlaneLogExport::owed_by`]), and record on each cluster what landed.
///
/// The one entry point every operation that reads a cluster calls, so the group a client
/// goes looking for after an enabling update is there whichever call it polled — see this
/// module's header for why the export is owed by the cluster rather than by one call.
/// Nothing is owed by a cluster that enables no log type, which is the case this walks past
/// without asking CloudWatch Logs anything.
///
/// Call it with NO store guard held: it awaits.
pub async fn export_owed_control_plane_logs(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    state: &::engine::ServiceState<ComAmazonawsEksAwswesleyfrontendState>,
    region: &str,
) {
    let owed: Vec<((String, String), ControlPlaneLogExport)> = {
        let mut guard = state.lock();
        // The completion of a `logging` update is what puts the configuration on the
        // cluster, and it was due at its own instant whether or not anybody called, so it
        // is materialised here before the owed exports are read off the records.
        guard.materialize_due_transitions(SystemTime::now());
        guard
            .clusters
            .iter()
            .filter(|((cluster_region, _), _)| cluster_region == region)
            .filter_map(|(key, record)| {
                ControlPlaneLogExport::owed_by(record).map(|export| (key.clone(), export))
            })
            .collect()
    };
    for (key, export) in owed {
        export_control_plane_logs_and_record(ctx, state, region, &key, &export).await;
    }
}

/// Make `export` and record the types that landed on the cluster `key` names, so a later
/// read owes them no second time.
///
/// The ONE path the two operations that change a cluster's logging configuration export
/// through, beside [`export_owed_control_plane_logs`] above: a type is recorded as exported
/// only once CloudWatch Logs has taken its stream, so one it refused stays owed.
///
/// Call it with NO store guard held: it awaits.
pub async fn export_control_plane_logs_and_record(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    state: &::engine::ServiceState<ComAmazonawsEksAwswesleyfrontendState>,
    region: &str,
    key: &(String, String),
    export: &ControlPlaneLogExport,
) {
    let exported = export_control_plane_logs(ctx, region, export).await;
    if exported.is_empty() {
        return;
    }
    // The cluster is read back rather than assumed: a `DeleteCluster` could have removed it
    // while the export was being made, and a record that is gone owes nothing.
    let mut guard = state.lock();
    if let Some(record) = guard.clusters.get_mut(key) {
        record.record_exported_log_types(&exported);
    }
}

/// Export the control plane logs of the types this cluster has enabled: create the
/// cluster's log group if CloudWatch Logs does not hold it yet, then open the stream of
/// each enabled type and deliver one record to it ([`export_record`]).
///
/// Answers the types whose group and stream CloudWatch Logs now HOLDS, which is what
/// [`export_control_plane_logs_and_record`] records on the cluster: a type CloudWatch Logs
/// would not take is not among them and stays owed.
///
/// Best effort and never fatal — see this module's header. Call it with NO store guard
/// held: it awaits.
pub async fn export_control_plane_logs(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    export: &ControlPlaneLogExport,
) -> Vec<&'static str> {
    // No Region label, so no endpoint to reach CloudWatch Logs at: the same silence an
    // unmounted CloudWatch Logs leaves, which a cluster call is not refused over.
    let Some(endpoint) = peer_endpoint("logs", region) else {
        return Vec::new();
    };
    let logs = callback_client!(aws_sdk_cloudwatchlogs, ctx, endpoint, region);
    let log_group = cluster_log_group_name(&export.cluster_name);
    let exported_at = SystemTime::now();
    let mut exported: Vec<&'static str> = Vec::new();
    for log_type in &export.log_types {
        let Some(log_stream) = control_plane_log_stream_name(log_type, &export.resource_id) else {
            continue;
        };
        let Some(component) = control_plane_component(log_type) else {
            continue;
        };
        let record = export_record(
            &export.cluster_name,
            log_type,
            component,
            export.version.as_deref(),
            export.platform_version.as_deref(),
        );
        if deliver_log_record(
            &logs,
            &export.cluster_name,
            &log_group,
            &log_stream,
            record,
            exported_at,
        )
        .await
        {
            exported.push(log_type);
        }
    }
    exported
}

/// Deliver ONE record to `log_stream` of `log_group`: create the group if CloudWatch
/// Logs does not hold it yet, open the stream if it is not open, then put the event.
///
/// `true` once CloudWatch Logs holds the group and the stream, which is the state the
/// export owes — the record itself is one log event, and losing it loses no resource.
///
/// The ONE delivery sequence this service exports a control plane log record through —
/// the per-type record of a `logging` change ([`export_control_plane_logs`]) and the
/// per-request audit record of a cluster's API server
/// ([`super::kubernetes_audit::AuditDelivery`]) — so the two cannot drift in which
/// failures they tolerate. A group or a stream that is already there is exactly the
/// state the step owes; anything else is reported in this service's own tracing and
/// ends the delivery, because the export is best effort and never fatal.
pub(super) async fn deliver_log_record(
    logs: &::aws_sdk_cloudwatchlogs::Client,
    cluster_name: &str,
    log_group: &str,
    log_stream: &str,
    record: String,
    at: SystemTime,
) -> bool {
    let created = logs
        .create_log_group()
        .log_group_name(log_group)
        .send()
        .await;
    if let Err(error) = created {
        if !already_there(&error) {
            ::tracing::warn!(
                %cluster_name,
                %log_group,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the cluster's control plane log group could not be created, so this \
                 record is not exported"
            );
            return false;
        }
    }
    let opened = logs
        .create_log_stream()
        .log_group_name(log_group)
        .log_stream_name(log_stream)
        .send()
        .await;
    if let Err(error) = opened {
        if !already_there(&error) {
            ::tracing::warn!(
                %cluster_name,
                %log_group,
                %log_stream,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the log stream this record exports under could not be opened"
            );
            return false;
        }
    }
    let Ok(event) = ::aws_sdk_cloudwatchlogs::types::InputLogEvent::builder()
        .timestamp(epoch_millis(at))
        .message(record)
        .build()
    else {
        return true;
    };
    let delivered = logs
        .put_log_events()
        .log_group_name(log_group)
        .log_stream_name(log_stream)
        .log_events(event)
        .send()
        .await;
    if let Err(error) = delivered {
        ::tracing::warn!(
            %cluster_name,
            %log_group,
            %log_stream,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "this control plane log record could not be delivered"
        );
    }
    true
}

/// Whether a create CloudWatch Logs refused is one it refused because the group or the
/// stream is ALREADY there, which is the state the delivery owes either way.
fn already_there<Error, Response>(
    error: &::aws_sdk_cloudwatchlogs::error::SdkError<Error, Response>,
) -> bool
where
    Error: AlreadyExists,
{
    error
        .as_service_error()
        .is_some_and(AlreadyExists::is_already_there)
}

/// The one question [`already_there`] asks of the two errors it is given, each of which
/// models the conflict as its own variant.
pub(super) trait AlreadyExists {
    fn is_already_there(&self) -> bool;
}

impl AlreadyExists for ::aws_sdk_cloudwatchlogs::operation::create_log_group::CreateLogGroupError {
    fn is_already_there(&self) -> bool {
        self.is_resource_already_exists_exception()
    }
}

impl AlreadyExists
    for ::aws_sdk_cloudwatchlogs::operation::create_log_stream::CreateLogStreamError
{
    fn is_already_there(&self) -> bool {
        self.is_resource_already_exists_exception()
    }
}

/// `at` in milliseconds since the Unix epoch, which is the stamp a log event carries.
///
/// An instant before the epoch, which no log event can carry, clamps to the epoch rather
/// than wrapping.
pub(super) fn epoch_millis(at: SystemTime) -> i64 {
    at.duration_since(::std::time::SystemTime::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}
