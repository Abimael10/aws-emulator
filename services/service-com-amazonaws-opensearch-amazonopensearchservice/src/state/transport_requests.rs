// SPDX-License-Identifier: CC0-1.0

//! The TRANSPORT REQUEST the cluster carries one action in — the type an audit record of a
//! transport-layer decision names it by.
//!
//! A decision of the cluster's transport layer is about an ACTION, and the engine carries
//! every action in a request object of its own: a document read travels as the engine's get
//! request, a document write as its index request, a mapping change as its put-mapping
//! request. The security plugin's audit record of such a decision therefore states the TYPE
//! of that request beside the privilege the action asked for — which is what tells a reader
//! of the log WHICH operation the cluster decided on, since one privilege can be asked for by
//! more than one route and the privilege alone does not name the request. A record of a
//! REST-layer decision carries no such member: at that layer there is no transport request
//! yet, only the path and the method the caller sent ([`super::audit`]).
//!
//! So this module is the ONE mapping from a transport action this plane takes decisions on
//! ([`super::fgac`]'s `ACTION_*`) to the name of the request the engine carries it in, keyed
//! by the action constants themselves so no action is spelled twice. The names are the
//! engine's own request types for those actions — the core's for the index, document, cluster
//! and snapshot APIs, and each plugin's own for the index-management, alerting and
//! notifications APIs — which is what the plugin reports, and the names are stable because
//! they are part of what every audit reader of a real cluster already reads.
//!
//! An action with no entry leaves the member out rather than reporting a type nothing carries
//! it; every action [`crate::service::domain_rest`]'s guard takes a decision for has one.

use super::fgac;

/// The request type the engine carries each transport action in: the action as the security
/// plugin names it ([`fgac`]), and the type of the request object the engine moves it in.
///
/// Keyed by the action CONSTANTS, so the action strings live in one place
/// ([`fgac`]) and a new guarded action is added here beside the constant it guards.
const TRANSPORT_REQUEST_TYPES: &[(&str, &str)] = &[
    // The cluster's own reads and writes.
    (fgac::ACTION_CLUSTER_HEALTH, "ClusterHealthRequest"),
    (fgac::ACTION_CLUSTER_STATE, "ClusterStateRequest"),
    (fgac::ACTION_NODES_INFO, "NodesInfoRequest"),
    (
        fgac::ACTION_GET_CLUSTER_SETTINGS,
        "ClusterGetSettingsRequest",
    ),
    (
        fgac::ACTION_UPDATE_CLUSTER_SETTINGS,
        "ClusterUpdateSettingsRequest",
    ),
    (fgac::ACTION_GET_TASK, "GetTaskRequest"),
    // The ingest pipelines a write may name.
    (fgac::ACTION_PUT_PIPELINE, "PutPipelineRequest"),
    (fgac::ACTION_GET_PIPELINE, "GetPipelineRequest"),
    (fgac::ACTION_DELETE_PIPELINE, "DeletePipelineRequest"),
    (fgac::ACTION_SIMULATE_PIPELINE, "SimulatePipelineRequest"),
    // The index-management plugin's policies.
    (fgac::ACTION_PUT_ISM_POLICY, "IndexPolicyRequest"),
    (fgac::ACTION_GET_ISM_POLICY, "GetPolicyRequest"),
    (fgac::ACTION_DELETE_ISM_POLICY, "DeletePolicyRequest"),
    // The alerting plugin's monitors, their runs and the alerts they leave.
    (fgac::ACTION_WRITE_MONITOR, "IndexMonitorRequest"),
    (fgac::ACTION_GET_MONITOR, "GetMonitorRequest"),
    (fgac::ACTION_DELETE_MONITOR, "DeleteMonitorRequest"),
    (fgac::ACTION_EXECUTE_MONITOR, "ExecuteMonitorRequest"),
    (fgac::ACTION_GET_ALERTS, "GetAlertsRequest"),
    (fgac::ACTION_WRITE_DESTINATION, "IndexDestinationRequest"),
    (fgac::ACTION_GET_DESTINATION, "GetDestinationsRequest"),
    (fgac::ACTION_DELETE_DESTINATION, "DeleteDestinationRequest"),
    // The notifications plugin's channel configurations.
    (
        fgac::ACTION_CREATE_NOTIFICATION_CONFIG,
        "CreateNotificationConfigRequest",
    ),
    (
        fgac::ACTION_GET_NOTIFICATION_CONFIG,
        "GetNotificationConfigRequest",
    ),
    (
        fgac::ACTION_UPDATE_NOTIFICATION_CONFIG,
        "UpdateNotificationConfigRequest",
    ),
    (
        fgac::ACTION_DELETE_NOTIFICATION_CONFIG,
        "DeleteNotificationConfigRequest",
    ),
    // The documents an index holds.
    (fgac::ACTION_GET_DOCUMENT, "GetRequest"),
    (fgac::ACTION_INDEX_DOCUMENT, "IndexRequest"),
    (fgac::ACTION_UPDATE_DOCUMENT, "UpdateRequest"),
    (fgac::ACTION_DELETE_DOCUMENT, "DeleteRequest"),
    (fgac::ACTION_DELETE_BY_QUERY, "DeleteByQueryRequest"),
    (fgac::ACTION_BULK, "BulkRequest"),
    // The reads over them. A count runs the same search the search API runs, over the same
    // request, and reports how many documents it selected rather than the documents.
    (fgac::ACTION_SEARCH, "SearchRequest"),
    (fgac::ACTION_COUNT, "SearchRequest"),
    // The indexes themselves, their settings, their mappings and the aliases that stand for
    // them. `ACTION_GET_INDEX_SETTINGS` and `ACTION_UPDATE_INDEX_SETTINGS` are the same two
    // actions under their index-API names, so they resolve through these entries.
    (fgac::ACTION_CREATE_INDEX, "CreateIndexRequest"),
    (fgac::ACTION_GET_INDEX, "GetIndexRequest"),
    (fgac::ACTION_DELETE_INDEX, "DeleteIndexRequest"),
    (fgac::ACTION_GET_SETTINGS, "GetSettingsRequest"),
    (fgac::ACTION_UPDATE_SETTINGS, "UpdateSettingsRequest"),
    (fgac::ACTION_GET_MAPPING, "GetMappingsRequest"),
    (fgac::ACTION_PUT_MAPPING, "PutMappingRequest"),
    (fgac::ACTION_ALIASES, "IndicesAliasesRequest"),
    (fgac::ACTION_GET_ALIASES, "GetAliasesRequest"),
    (fgac::ACTION_INDICES_STATS, "IndicesStatsRequest"),
    (fgac::ACTION_REFRESH, "RefreshRequest"),
    (fgac::ACTION_ANALYZE, "AnalyzeRequest"),
    // The templates an index is created from, and the component templates one is composed
    // of.
    (
        fgac::ACTION_PUT_TEMPLATE,
        "PutComposableIndexTemplateRequest",
    ),
    (
        fgac::ACTION_GET_TEMPLATE,
        "GetComposableIndexTemplateRequest",
    ),
    (
        fgac::ACTION_PUT_COMPONENT_TEMPLATE,
        "PutComponentTemplateRequest",
    ),
    (
        fgac::ACTION_GET_COMPONENT_TEMPLATE,
        "GetComponentTemplateRequest",
    ),
    // The security plugin's own configuration records.
    (fgac::ACTION_SECURITY_CONFIG, "ConfigUpdateRequest"),
    // The snapshot repositories a cluster registers and the snapshots it takes into one.
    (fgac::ACTION_PUT_REPOSITORY, "PutRepositoryRequest"),
    (fgac::ACTION_GET_REPOSITORY, "GetRepositoriesRequest"),
    (fgac::ACTION_DELETE_REPOSITORY, "DeleteRepositoryRequest"),
    (fgac::ACTION_CREATE_SNAPSHOT, "CreateSnapshotRequest"),
    (fgac::ACTION_GET_SNAPSHOT, "GetSnapshotsRequest"),
    (fgac::ACTION_DELETE_SNAPSHOT, "DeleteSnapshotRequest"),
    (fgac::ACTION_RESTORE_SNAPSHOT, "RestoreSnapshotRequest"),
];

/// The type of the request the engine carries `action` in, or `None` for an action this
/// mapping does not name.
#[must_use]
pub fn transport_request_type(action: &str) -> Option<&'static str> {
    TRANSPORT_REQUEST_TYPES
        .iter()
        .find(|(named, _)| *named == action)
        .map(|(_, request_type)| *request_type)
}
