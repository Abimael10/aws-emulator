// SPDX-License-Identifier: CC0-1.0

//! The documented stop / restart / modify lifecycle of an elastic cluster, driven the way
//! a polling client drives it, end to end over HTTP.
//!
//! The rule these suites assert: **a status transition applied to a cluster this store
//! already holds reports its transient status only in the response of the call that
//! started it, and the settled status that transient one leads to is owed from the instant
//! that call answers — so the FIRST reporting call after it is answered the settled
//! cluster, whatever interval the client polls on.** `StopCluster` "stops the running
//! elastic cluster" and its cluster "must be in the available state", `StartCluster`
//! "restarts the stopped elastic cluster", and `GetCluster` / `ListClusters` report the
//! same required `Cluster`/`ClusterInList` with its required `status`
//! (`smithy_query(query="operation", operation="StopCluster,StartCluster,UpdateCluster,\
//! GetCluster,ListClusters")`). So a client doing the whole documented flow — stop and
//! wait for the cluster to be stopped, restart it and wait for it to be available, then
//! modify it and wait for the modification — looks exactly ONCE per transition, and the
//! refusal a modification of a stopped cluster meets (409 `ConflictException`) neither
//! changes the stored cluster nor disturbs that pacing.
//!
//! The rows differ in the shape of the cluster the flow runs against (shard count, shard
//! capacity, instances per shard, auth type) and in the members the flow's modification
//! carries, so the pacing is a property of a transition rather than of one record or one
//! modification. The second suite varies the READER the client polls with: `ListClusters`
//! alone paces the same as `GetCluster` alone.
//!
//! What this file does NOT assert is that a transition is instantaneous for a call that
//! ACTS on the cluster: a change stays in flight against the next acting call, which
//! `all/cluster_lifecycle.rs` pins.
//!
//! Each suite in this crate carries its own request harness (see
//! `all/cluster_lifecycle.rs`): the helpers below are this module's local copy rather
//! than a second implementation of anything the service owns.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// The region every suite here signs with.
const REGION: &str = "eu-west-2";

/// A SigV4 `Authorization` header for this service in [`REGION`]: the credential scope is
/// what the router routes on and what the handlers read the region from.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/docdb-elastic/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One emulator: the shared engine plus a router carrying only this service.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_docdbelastic_chimeradblionfishservicelambda::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// Percent-encode one `@httpLabel` value, the way an AWS SDK does: a label is a single
/// path segment and an ARN carries both `:` and `/`, so neither may survive raw.
fn label(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Send one request and return its status and parsed JSON body (`Value::Null` when the
/// body is empty).
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://docdb-elastic.{REGION}.amazonaws.com{path}"))
        .header("host", format!("docdb-elastic.{REGION}.amazonaws.com"))
        .header("authorization", authorization())
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "{method} {path}: body is not JSON ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

/// The wire error code of an error response (`__type`, bare or namespaced).
fn error_code(body: &Value) -> String {
    let raw = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>");
    raw.rsplit('#').next().unwrap_or(raw).to_string()
}

/// `POST /cluster` — one `CreateCluster` call, asserted accepted, returning the minted
/// cluster.
async fn create(router: &MultiServiceRouter, case: &str, body: Value) -> Value {
    let (status, response) = call(router, "POST", "/cluster", Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateCluster must answer 200, got {status} with {response}"
    );
    response["cluster"].clone()
}

/// `POST /cluster/{clusterArn}/{action}` — one `StopCluster` or `StartCluster` call,
/// asserted accepted, returning the cluster it reports.
async fn act(router: &MultiServiceRouter, case: &str, cluster_arn: &str, action: &str) -> Value {
    let path = format!("/cluster/{}/{action}", label(cluster_arn));
    let (status, response) = call(router, "POST", &path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the {action} must answer 200, got {status} with {response}"
    );
    response["cluster"].clone()
}

/// `PUT /cluster/{clusterArn}` — one `UpdateCluster` call, with whatever it answered.
async fn update(router: &MultiServiceRouter, cluster_arn: &str, patch: Value) -> (u16, Value) {
    let path = format!("/cluster/{}", label(cluster_arn));
    call(router, "PUT", &path, Some(patch)).await
}

/// `GET /cluster/{clusterArn}` — ONE `GetCluster` look, asserted readable. Every caller
/// below looks exactly once per transition: that single look is what the pacing rule is
/// about, so none of them loops.
async fn get_cluster(router: &MultiServiceRouter, case: &str, cluster_arn: &str) -> Value {
    let path = format!("/cluster/{}", label(cluster_arn));
    let (status, response) = call(router, "GET", &path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCluster must answer 200, got {status} with {response}"
    );
    response["cluster"].clone()
}

/// The status `ListClusters` reports for `cluster_arn` on ONE look.
async fn listed_status(router: &MultiServiceRouter, case: &str, cluster_arn: &str) -> String {
    let (status, response) = call(router, "GET", "/clusters", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListClusters must answer 200, got {status} with {response}"
    );
    response["clusters"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no `clusters` array in {response}"))
        .iter()
        .find(|summary| summary["clusterArn"].as_str() == Some(cluster_arn))
        .and_then(|summary| summary["status"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: the cluster is missing from {response}"))
        .to_string()
}

/// A `CreateCluster` body with every required member, plus whatever `extra` carries.
///
/// The capacities and counts the rows pick come from the documented domains
/// (`CreateClusterInput$shardCapacity` — "Allowed values are 2, 4, 8, 16, 32, 64";
/// `$shardCount` — "Maximum is 32").
fn create_body(name: &str, extra: &Value) -> Value {
    let mut body = json!({
        "clusterName": name,
        "authType": "PLAIN_TEXT",
        "adminUserName": "owner",
        "adminUserPassword": "Secret123!",
        "shardCapacity": 2,
        "shardCount": 1,
    });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        body[member] = value;
    }
    body
}

/// The fixed sentence a cluster whose status does not admit a modification is refused
/// with, as the service words it for every such cluster.
const CLUSTER_IN_USE: &str = "Can't perform operation on cluster as it's in use";

/// One row of the flow: the cluster to run it against, and the modification it applies
/// once the cluster has been stopped, refused and restarted.
struct FlowRow {
    case: &'static str,
    /// The `CreateCluster` members beyond the required minimum.
    extra: Value,
    /// The `UpdateCluster` body, applied to the stopped cluster (refused) and again to the
    /// restarted one (accepted).
    patch: Value,
}

#[tokio::test]
async fn the_documented_stop_restart_update_flow_settles_with_one_look_per_transition() {
    // The rows differ in the shape of the cluster the flow runs against and in the
    // members the modification carries: one member, a resize of the shard count, and
    // several members at once.
    let rows: Vec<FlowRow> = vec![
        FlowRow {
            case: "one shard, the smallest capacity, instances per shard raised",
            extra: json!({"shardInstanceCount": 1}),
            patch: json!({"shardInstanceCount": 2}),
        },
        FlowRow {
            case: "several shards, the capacity raised",
            extra: json!({"shardCount": 3, "shardCapacity": 4}),
            patch: json!({"shardCapacity": 16}),
        },
        FlowRow {
            case: "the shard count resized",
            extra: json!({"shardCount": 2, "shardCapacity": 8}),
            patch: json!({"shardCount": 4}),
        },
        FlowRow {
            case: "the largest published capacity, several members modified",
            extra: json!({"shardCapacity": 64, "shardInstanceCount": 3}),
            patch: json!({
                "backupRetentionPeriod": 14,
                "preferredBackupWindow": "02:00-03:00",
                "subnetIds": ["subnet-paced"],
            }),
        },
        FlowRow {
            case: "the other auth type, the network configuration modified",
            extra: json!({"authType": "SECRET_ARN", "tags": {"env": "test"}}),
            patch: json!({"vpcSecurityGroupIds": ["sg-paced"]}),
        },
    ];

    let router = emulator();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let created = create(&router, case, create_body(&format!("flow{index}"), &row.extra)).await;
        let cluster_arn = created["clusterArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no `clusterArn` in {created}"))
            .to_string();

        // Transition 1, the create: its own response carries the transient status, and one
        // look reports the cluster the flow acts on.
        let active = get_cluster(&router, case, &cluster_arn).await;
        assert_eq!(
            active["status"].as_str(),
            Some("ACTIVE"),
            "case {case:?}: one look must report the created cluster available, got {active}"
        );

        // Transition 2, the stop: STOPPING in the stop's own response, STOPPED to the one
        // look that follows it.
        let stopping = act(&router, case, &cluster_arn, "stop").await;
        assert_eq!(
            stopping["status"].as_str(),
            Some("STOPPING"),
            "case {case:?}: the stop's own response carries the transient status, got {stopping}"
        );
        let stopped = get_cluster(&router, case, &cluster_arn).await;
        assert_eq!(
            stopped["status"].as_str(),
            Some("STOPPED"),
            "case {case:?}: one look must report the stop settled, got {stopped}"
        );

        // The modification a stopped cluster refuses: 409, the service's own sentence, and
        // nothing written.
        let (status, refused) = update(&router, &cluster_arn, row.patch.clone()).await;
        assert_eq!(
            status, 409,
            "case {case:?}: modifying a stopped cluster must answer 409, got {status} with \
             {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ConflictException",
            "case {case:?}: expected a ConflictException, got {refused}"
        );
        assert_eq!(
            refused["message"].as_str(),
            Some(CLUSTER_IN_USE),
            "case {case:?}: expected the in-use sentence {CLUSTER_IN_USE:?}, got {refused}"
        );
        let still_stopped = get_cluster(&router, case, &cluster_arn).await;
        assert_eq!(
            still_stopped["status"].as_str(),
            Some("STOPPED"),
            "case {case:?}: the refused modification leaves the cluster stopped, got \
             {still_stopped}"
        );
        for (member, value) in row.patch.as_object().expect("a patch object") {
            assert_eq!(
                still_stopped.get(member),
                stopped.get(member),
                "case {case:?}: the refused modification must not apply `{member}` ({value}), got \
                 {:?} where the stored cluster has {:?}",
                still_stopped.get(member),
                stopped.get(member)
            );
        }

        // Transition 3, the restart: STARTING in the start's own response, available to
        // the one look that follows it.
        let starting = act(&router, case, &cluster_arn, "start").await;
        assert_eq!(
            starting["status"].as_str(),
            Some("STARTING"),
            "case {case:?}: the start's own response carries the transient status, got {starting}"
        );
        let restarted = get_cluster(&router, case, &cluster_arn).await;
        assert_eq!(
            restarted["status"].as_str(),
            Some("ACTIVE"),
            "case {case:?}: one look must report the restart settled, got {restarted}"
        );
        // The stop and the restart moved the status and nothing else.
        for member in [
            "clusterName",
            "clusterArn",
            "clusterEndpoint",
            "createTime",
            "authType",
            "shardCapacity",
            "shardCount",
            "shardInstanceCount",
            "kmsKeyId",
            "preferredMaintenanceWindow",
        ] {
            assert_eq!(
                restarted.get(member),
                active.get(member),
                "case {case:?}: `{member}` must survive the stop and the restart, expected {:?}, \
                 got {:?}",
                active.get(member),
                restarted.get(member)
            );
        }
        assert_eq!(
            restarted["shards"].as_array().map(Vec::len),
            active["shards"].as_array().map(Vec::len),
            "case {case:?}: the shard topology must survive the stop and the restart, got \
             {restarted}"
        );

        // Transition 4, the identical modification, which the restarted cluster now takes:
        // the positive control that proves the refusal above was a state gate.
        let (status, accepted) = update(&router, &cluster_arn, row.patch.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the identical modification must be taken by the restarted cluster, \
             got {status} with {accepted}"
        );
        let updating = accepted["cluster"].clone();
        assert_eq!(
            updating["status"].as_str(),
            Some("UPDATING"),
            "case {case:?}: the modification's own response carries the transient status, got \
             {updating}"
        );
        let applied = get_cluster(&router, case, &cluster_arn).await;
        assert_eq!(
            applied["status"].as_str(),
            Some("ACTIVE"),
            "case {case:?}: one look must report the modification settled, got {applied}"
        );
        for (member, value) in row.patch.as_object().expect("a patch object") {
            assert_eq!(
                applied.get(member),
                Some(value),
                "case {case:?}: `{member}` must be the modification's {value}, got {:?}",
                applied.get(member)
            );
        }
        // Every shard moved with the cluster, and the settled status is where it stays.
        for shard in applied["shards"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no `shards` in {applied}"))
        {
            assert_eq!(
                shard["status"].as_str(),
                Some("ACTIVE"),
                "case {case:?}: a settled cluster's shards settle with it, got {shard}"
            );
        }
        let again = get_cluster(&router, case, &cluster_arn).await;
        assert_eq!(
            again["status"].as_str(),
            Some("ACTIVE"),
            "case {case:?}: a settled cluster stays settled, got {again}"
        );
    }
}

#[tokio::test]
async fn a_client_that_polls_the_listing_sees_each_transition_settle_on_its_first_look() {
    // The same pacing, for a client whose waiter looks through the LISTING rather than
    // through the per-cluster read: `ListClusters` reports the same stored status, so one
    // look settles each transition there too. No `GetCluster` is issued until the flow is
    // over, so nothing else can have settled the cluster first.
    let case = "a client polling the listing";
    let router = emulator();
    let created = create(
        &router,
        case,
        create_body("listingpaced", &json!({"shardCount": 2, "shardCapacity": 4})),
    )
    .await;
    let cluster_arn = created["clusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no `clusterArn` in {created}"))
        .to_string();

    let transitions: &[(&str, &str, &str)] = &[
        // (what the flow does, the transient status its own response carries, the status
        // the first listing after it reports)
        ("create", "CREATING", "ACTIVE"),
        ("stop", "STOPPING", "STOPPED"),
        ("start", "STARTING", "ACTIVE"),
        ("update", "UPDATING", "ACTIVE"),
    ];

    for (action, transient, settled) in transitions {
        let reported = match *action {
            "create" => created["status"].as_str().map(str::to_string),
            "update" => {
                let (status, accepted) =
                    update(&router, &cluster_arn, json!({"shardInstanceCount": 2})).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the modification must be taken, got {status} with {accepted}"
                );
                accepted["cluster"]["status"].as_str().map(str::to_string)
            }
            other => act(&router, case, &cluster_arn, other).await["status"]
                .as_str()
                .map(str::to_string),
        };
        assert_eq!(
            reported.as_deref(),
            Some(*transient),
            "case {case:?}: the {action}'s own response must carry {transient:?}, got {reported:?}"
        );
        let listed = listed_status(&router, case, &cluster_arn).await;
        assert_eq!(
            listed, *settled,
            "case {case:?}: one look at the listing after the {action} must report {settled:?}, \
             got {listed:?}"
        );
    }

    // The flow's modification really was applied, read back through `GetCluster`.
    let applied = get_cluster(&router, case, &cluster_arn).await;
    assert_eq!(
        applied["shardInstanceCount"].as_i64(),
        Some(2),
        "case {case:?}: the modification the listing saw settle must be stored, got {applied}"
    );
}
