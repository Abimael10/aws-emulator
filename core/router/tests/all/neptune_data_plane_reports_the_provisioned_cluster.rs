// SPDX-License-Identifier: CC0-1.0

//! The graph data plane reports the engine the CONTROL PLANE provisioned for the cluster it
//! answers for.
//!
//! An Amazon Neptune cluster's data plane is the graph API (restJson1, signing name
//! `neptune-db`), addressed at the cluster's own connection endpoint. `GET /status` reports
//! on the ENGINE behind that endpoint: `dbEngineVersion` is "the Neptune engine version
//! running on the DB cluster", `gremlin`/`sparql`/`opencypher` are "the version of each
//! query language available on the cluster", and `features` carries "status information
//! about the features enabled on the cluster". Every one of those is a fact of the cluster a
//! client CREATED — its `EngineVersion`, its `EnableIAMDatabaseAuthentication`, the logs its
//! `EnableCloudwatchLogsExports` publishes — and the data plane settles none of them, so a
//! status that answers constants reports an engine nobody provisioned: a client gating a
//! feature on the engine line it reads back, or asking whether IAM authentication is on,
//! branches the wrong way.
//!
//! So each row below CREATES a cluster through the control plane and then reads the status
//! through the published endpoint, asserting that what the status reports is what the create
//! settled. The rows differ along what the rule generalises over: the engine line
//! provisioned (two released lines, including the one this front door provisions by
//! default, and two releases of each line), whether IAM database authentication was asked
//! for, and which logs the cluster publishes.
//!
//! This suite needs the WHOLE router: the control plane that provisions the cluster (the
//! `rds` front door, which serves the Neptune control plane's model too) and the data plane
//! that answers for it (neptunedata) are two services, so a single-service test cannot see
//! the hand-off. Assembling the router costs stack, so it is built — and the requests driven
//! — on an explicitly sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::element;
use crate::rds_docdb_neptune_signing_name_routing::{authorization, API_VERSION, BOOT_STACK_SIZE};

/// The engine whose data plane is the graph API. `CreateDBCluster$Engine`: "Valid values: …
/// neptune".
const GRAPH_ENGINE: &str = "neptune";

/// The SigV4 signing name of the graph API, which is what routes a data-plane request to the
/// neptunedata service.
const GRAPH_API_SIGNING_NAME: &str = "neptune-db";

/// The signing name the RDS front door serves the Neptune control plane's model behind: an
/// AWS SDK Neptune client signs `rds`, because that model gives it no other name.
const CONTROL_PLANE_SIGNING_NAME: &str = "rds";

/// The endpoint the control-plane client is configured with, which is also the address the
/// cluster publishes for its graph API.
const CONTROL_PLANE_ENDPOINT: &str = "https://127.0.0.1:9297/";

/// One provisioned cluster, and what its graph API must then report.
struct Row {
    case: &'static str,
    /// `EngineVersion` the create asks for.
    engine_version: &'static str,
    /// Whether the create asks for IAM database authentication.
    iam_authentication: bool,
    /// The log types the create publishes, by the names the member takes (`audit`,
    /// `slowquery`).
    log_exports: &'static [&'static str],
    /// The `dbEngineVersion` the status must report: the provisioned version with the
    /// engine's release suffix.
    expected_engine_version: &'static str,
    /// The TinkerPop version that engine line carries.
    expected_gremlin: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the 1.4 line, with IAM authentication and both logs on",
        engine_version: "1.4.8.0",
        iam_authentication: true,
        log_exports: &["audit", "slowquery"],
        expected_engine_version: "1.4.8.0.R1",
        expected_gremlin: "tinkerpop-3.7.1",
    },
    Row {
        case: "the 1.3 line, with every switch off",
        engine_version: "1.3.2.0",
        iam_authentication: false,
        log_exports: &[],
        expected_engine_version: "1.3.2.0.R1",
        expected_gremlin: "tinkerpop-3.6.2",
    },
    Row {
        case: "the 1.3 line, publishing the audit log alone",
        engine_version: "1.3.0.0",
        iam_authentication: false,
        log_exports: &["audit"],
        expected_engine_version: "1.3.0.0.R1",
        expected_gremlin: "tinkerpop-3.6.2",
    },
    Row {
        case: "the first release of the 1.4 line, with IAM authentication on",
        engine_version: "1.4.0.0",
        iam_authentication: true,
        log_exports: &[],
        expected_engine_version: "1.4.0.0.R1",
        expected_gremlin: "tinkerpop-3.7.1",
    },
];

/// `CreateDBCluster` for `row`: the status and the XML answer.
async fn create_cluster(router: &MultiServiceRouter, row: &Row, identifier: &str) -> (u16, String) {
    let mut body = format!(
        "Action=CreateDBCluster&Version={API_VERSION}&DBClusterIdentifier={identifier}\
         &Engine={GRAPH_ENGINE}&EngineVersion={}&EnableIAMDatabaseAuthentication={}",
        row.engine_version, row.iam_authentication
    );
    for (index, log_type) in row.log_exports.iter().enumerate() {
        // The awsQuery member name of one entry of `EnableCloudwatchLogsExports`.
        body.push_str(&format!(
            "&EnableCloudwatchLogsExports.member.{}={log_type}",
            index + 1
        ));
    }
    let request = ::http::Request::builder()
        .method("POST")
        .uri(CONTROL_PLANE_ENDPOINT)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(CONTROL_PLANE_SIGNING_NAME))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed control-plane request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let rendered = String::from_utf8_lossy(response.body()).to_string();
    (status, rendered)
}

/// `GET /status` dialled at `authority` — the `host:port` the cluster published: the status
/// and the JSON body.
async fn engine_status(router: &MultiServiceRouter, authority: &str) -> (u16, Value) {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}/status"))
        .header("host", authority)
        .header("authorization", authorization(GRAPH_API_SIGNING_NAME))
        .body(bytes::Bytes::new())
        .expect("a well-formed data-plane request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let answered = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, answered)
}

#[test]
fn the_graph_status_reports_the_engine_the_control_plane_provisioned() {
    std::thread::Builder::new()
        .name("neptune-provisioned-engine".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                // IO and timers are on because a create reaches a service that opens a core
                // listener for the engines whose native wire this emulator speaks.
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            for (index, row) in ROWS.iter().enumerate() {
                let case = row.case;
                // The identifiers ascend with the rows, so the cluster a row just created is
                // the one its own status reads: the data plane answers for the cluster
                // provisioned last, and two creates in one clock tick are ordered by
                // identifier.
                let identifier = format!("emu-graph-engine-{index}");
                let (status, created) =
                    runtime.block_on(create_cluster(&router, row, &identifier));
                assert_eq!(
                    status, 200,
                    "case {case:?}: the create must be answered, got {status} with {created}"
                );
                let authority = format!(
                    "{}:{}",
                    element(&created, "Endpoint"),
                    element(&created, "Port")
                );

                let (status, answered) = runtime.block_on(engine_status(&router, &authority));
                assert_eq!(
                    status, 200,
                    "case {case:?}: GET https://{authority}/status must be answered, got \
                     {status} with {answered}"
                );
                assert_eq!(
                    answered["dbEngineVersion"].as_str(),
                    Some(row.expected_engine_version),
                    "case {case:?}: expected the provisioned engine version, got {answered}"
                );
                assert_eq!(
                    answered["gremlin"]["version"].as_str(),
                    Some(row.expected_gremlin),
                    "case {case:?}: expected the TinkerPop version that engine line carries, \
                     got {answered}"
                );
                // Each feature a create settles reports that cluster's own configuration, as
                // the BARE status string the engine reports it as.
                for (feature, enabled) in [
                    ("IAMAuthentication", row.iam_authentication),
                    ("AuditLog", row.log_exports.contains(&"audit")),
                    ("SlowQueryLogs", row.log_exports.contains(&"slowquery")),
                ] {
                    let expected = if enabled { "enabled" } else { "disabled" };
                    assert_eq!(
                        answered["features"][feature].as_str(),
                        Some(expected),
                        "case {case:?}: expected features.{feature} {expected:?}, got {answered}"
                    );
                }
            }
        })
        .expect("spawn the provisioned-engine thread")
        .join()
        .expect("the graph status must report the provisioned engine");
}
