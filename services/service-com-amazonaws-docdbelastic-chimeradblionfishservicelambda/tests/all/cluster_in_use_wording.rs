// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS the refusal of a modification a cluster cannot take yet.
//!
//! The rule this suite asserts: a modification refused because the cluster is not in its
//! settled available status answers 409 `ConflictException` (`smithy_query(query=
//! "operation", operation="UpdateCluster")`) whose `message` is ONE FIXED sentence —
//! the whole message being fixed text, with no part taken from the request and no part
//! taken from the cluster's record (not its name, not the status it is in) — while the
//! cluster is identified by `resourceId` and its class by `resourceType`. That is what
//! lets a caller recognise the one refusal it is expected to retry on, whichever
//! cluster, whichever in-flight change and whichever transient status produced it.
//!
//! The table below differs along what the rule generalises over: the status the cluster
//! is in when the modification arrives (carrying a change, carrying a resize, being
//! stopped, settled stopped, being restarted), the cluster's name, and the member the
//! refused modification carries. One sentence must serve every row, which is exactly
//! what a message built from the cluster's name or status cannot do.
//!
//! Each suite in this crate carries its own request harness (see
//! `all/cluster_lifecycle.rs`): the helpers below are the local copy rather than a
//! second implementation of anything the service itself owns.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// The region this suite signs with.
const REGION: &str = "eu-west-2";

/// The sentence this service answers when a cluster cannot take the operation asked of
/// it because it is not settled and available.
///
/// Recorded AWS behavior for an overlapping `UpdateCluster`: the entire message, with no
/// variable part.
const IN_USE_SENTENCE: &str = "Can't perform operation on cluster as it's in use";

/// The AWS resource type name the error body reports for a cluster.
///
/// Source: `smithy_query(query="shape", shape="ConflictException")` — `resourceType` is
/// "The type of the resource where there was an access conflict".
const CLUSTER_TYPE: &str = "Cluster";

/// How long a poll loop waits for a transition the clock owes before giving up. Generous
/// on purpose: the suite waits for the CONDITION, never for a fixed time, so a loaded
/// host cannot exhaust it.
const POLL_ATTEMPTS: usize = 1200;
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A SigV4 `Authorization` header for this service in [`REGION`].
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

/// Percent-encode one `@httpLabel` value, the way an AWS SDK does.
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

/// A `CreateCluster` body with every required member and nothing else.
fn minimal_create(name: &str) -> Value {
    json!({
        "clusterName": name,
        "authType": "PLAIN_TEXT",
        "adminUserName": "admin",
        "adminUserPassword": "Secret123!",
        "shardCapacity": 2,
        "shardCount": 1,
    })
}

/// `GET /cluster/{clusterArn}` — the stored cluster, asserted readable.
async fn get_cluster(router: &MultiServiceRouter, cluster_arn: &str) -> Value {
    let path = format!("/cluster/{}", label(cluster_arn));
    let (status, response) = call(router, "GET", &path, None).await;
    assert_eq!(
        status, 200,
        "case {cluster_arn}: GetCluster must answer 200, got {status} with {response}"
    );
    response["cluster"].clone()
}

/// Create one cluster and return its `clusterArn`, without waiting for it to settle.
async fn create_cluster(router: &MultiServiceRouter, name: &str) -> String {
    let (status, response) = call(router, "POST", "/cluster", Some(minimal_create(name))).await;
    assert_eq!(
        status, 200,
        "case {name:?}: CreateCluster must answer 200, got {status} with {response}"
    );
    response["cluster"]["clusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: no clusterArn in {response}"))
        .to_string()
}

/// Poll `GetCluster` until the cluster reports `wanted` — the status the clock owes it a
/// short span after the call that started its current transition.
async fn wait_for_status(router: &MultiServiceRouter, cluster_arn: &str, wanted: &str) -> Value {
    for _ in 0..POLL_ATTEMPTS {
        let cluster = get_cluster(router, cluster_arn).await;
        if cluster["status"].as_str() == Some(wanted) {
            return cluster;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {cluster_arn}: the cluster never reported {wanted}");
}

/// `PUT /cluster/{clusterArn}` — one `UpdateCluster` call.
async fn update(router: &MultiServiceRouter, cluster_arn: &str, patch: Value) -> (u16, Value) {
    let path = format!("/cluster/{}", label(cluster_arn));
    call(router, "PUT", &path, Some(patch)).await
}

/// `POST /cluster/{clusterArn}/stop`, asserted accepted.
async fn stop(router: &MultiServiceRouter, cluster_arn: &str) {
    let path = format!("/cluster/{}/stop", label(cluster_arn));
    let (status, response) = call(router, "POST", &path, None).await;
    assert_eq!(
        status, 200,
        "case {cluster_arn}: StopCluster must answer 200, got {status} with {response}"
    );
}

/// `POST /cluster/{clusterArn}/start`, asserted accepted.
async fn start(router: &MultiServiceRouter, cluster_arn: &str) {
    let path = format!("/cluster/{}/start", label(cluster_arn));
    let (status, response) = call(router, "POST", &path, None).await;
    assert_eq!(
        status, 200,
        "case {cluster_arn}: StartCluster must answer 200, got {status} with {response}"
    );
}

/// How a row gets its cluster out of the settled available status.
///
/// A cluster still being BUILT is not one of these: a mint reports its transient status
/// in its own response and the next caller to look is answered the settled cluster, so a
/// modification arriving after a create reaches an available cluster. What is left is
/// every status a change to an EXISTING cluster leaves it in, plus the settled status
/// that is not the available one.
#[derive(Debug, Clone, Copy)]
enum Setup {
    /// A cluster carrying a modification that is still in flight.
    WhileUpdating,
    /// A cluster carrying a resize of its shard count that is still in flight.
    WhileResizing,
    /// A cluster whose stop is still in flight.
    WhileStopping,
    /// A cluster whose stop has settled: not transient, but not available either.
    WhenStopped,
    /// A cluster whose restart is still in flight.
    WhileStarting,
}

/// One row: the cluster to build, the status it must report when the modification
/// arrives, and the modification that must be refused.
struct Row {
    case: &'static str,
    name: &'static str,
    setup: Setup,
    status: &'static str,
    patch: Value,
}

/// Build the row's cluster and leave it in the status the row names, returning its ARN.
async fn arrange(router: &MultiServiceRouter, row: &Row) -> String {
    let cluster_arn = create_cluster(router, row.name).await;
    match row.setup {
        Setup::WhileUpdating => {
            wait_for_status(router, &cluster_arn, "ACTIVE").await;
            let (status, response) = update(router, &cluster_arn, json!({"shardCapacity": 4})).await;
            assert_eq!(
                status, 200,
                "case {:?}: the first modification must be accepted, got {status} with {response}",
                row.case
            );
        }
        Setup::WhileResizing => {
            wait_for_status(router, &cluster_arn, "ACTIVE").await;
            let (status, response) = update(router, &cluster_arn, json!({"shardCount": 3})).await;
            assert_eq!(
                status, 200,
                "case {:?}: the first modification must be accepted, got {status} with {response}",
                row.case
            );
        }
        Setup::WhileStopping => {
            wait_for_status(router, &cluster_arn, "ACTIVE").await;
            stop(router, &cluster_arn).await;
        }
        Setup::WhenStopped => {
            wait_for_status(router, &cluster_arn, "ACTIVE").await;
            stop(router, &cluster_arn).await;
            wait_for_status(router, &cluster_arn, "STOPPED").await;
        }
        Setup::WhileStarting => {
            wait_for_status(router, &cluster_arn, "ACTIVE").await;
            stop(router, &cluster_arn).await;
            wait_for_status(router, &cluster_arn, "STOPPED").await;
            start(router, &cluster_arn).await;
        }
    }
    cluster_arn
}

/// The rows: every way a cluster can be unavailable for a modification, each with its
/// own name and its own refused member.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a cluster carrying a modification still in flight",
            name: "fleetalpha",
            setup: Setup::WhileUpdating,
            status: "UPDATING",
            patch: json!({"shardCapacity": 8}),
        },
        Row {
            case: "a cluster carrying a shard-count resize still in flight",
            name: "fleetbravo",
            setup: Setup::WhileResizing,
            status: "UPDATING",
            patch: json!({"shardCount": 7}),
        },
        Row {
            case: "a cluster being stopped",
            name: "fleetcharlie",
            setup: Setup::WhileStopping,
            status: "STOPPING",
            patch: json!({"authType": "SECRET_ARN"}),
        },
        Row {
            case: "a cluster settled stopped",
            name: "fleetdelta",
            setup: Setup::WhenStopped,
            status: "STOPPED",
            patch: json!({"subnetIds": ["subnet-while-stopped"]}),
        },
        Row {
            case: "a cluster being restarted",
            name: "fleetecho",
            setup: Setup::WhileStarting,
            status: "STARTING",
            patch: json!({"backupRetentionPeriod": 14}),
        },
    ]
}

/// Check one refusal against the rule, returning the `message` it carried.
fn assert_in_use(case: &str, status: u16, body: &Value, cluster_arn: &str, row: &Row) -> String {
    assert_eq!(
        status, 409,
        "case {case:?}: a cluster that cannot take the modification is HTTP 409, got {status} \
         with {body}"
    );
    assert_eq!(
        error_code(body),
        "ConflictException",
        "case {case:?}: expected a ConflictException, got {body}"
    );
    let message = body["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the refusal must carry `message`, got {body}"))
        .to_string();
    assert_eq!(
        message, IN_USE_SENTENCE,
        "case {case:?}: the refusal must be the fixed sentence {IN_USE_SENTENCE:?}, got {message:?}"
    );
    // Nothing from the cluster's record may appear in the message: not its name, not the
    // status it is in, not its ARN — those travel in the structured members.
    assert!(
        !message.contains(row.name),
        "case {case:?}: the message must not state the cluster's name, found {:?} in {message:?}",
        row.name
    );
    assert!(
        !message.contains(row.status),
        "case {case:?}: the message must not state the cluster's status, found {:?} in \
         {message:?}",
        row.status
    );
    assert!(
        !message.contains(cluster_arn),
        "case {case:?}: the message must not state the cluster's ARN, got {message:?}"
    );
    assert_eq!(
        body["resourceId"].as_str(),
        Some(cluster_arn),
        "case {case:?}: resourceId must name the cluster the conflict is about, got {body}"
    );
    assert_eq!(
        body["resourceType"].as_str(),
        Some(CLUSTER_TYPE),
        "case {case:?}: resourceType must be {CLUSTER_TYPE:?}, got {body}"
    );
    message
}

#[tokio::test]
async fn a_modification_of_an_unavailable_cluster_carries_the_fixed_in_use_sentence() {
    let router = emulator();
    for row in rows() {
        let cluster_arn = arrange(&router, &row).await;
        let observed = get_cluster(&router, &cluster_arn).await;
        assert_eq!(
            observed["status"].as_str(),
            Some(row.status),
            "case {:?}: the row's cluster must report {:?} when the modification arrives, got \
             {observed}",
            row.case,
            row.status
        );

        let (status, refused) = update(&router, &cluster_arn, row.patch.clone()).await;
        assert_in_use(row.case, status, &refused, &cluster_arn, &row);

        // The refusal is the whole effect: nothing the refused request carried was
        // applied.
        let stored = get_cluster(&router, &cluster_arn).await;
        for (member, value) in row.patch.as_object().expect("a patch object") {
            assert_ne!(
                stored.get(member),
                Some(value),
                "case {:?}: a refused modification must not apply `{member}`, got {stored}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn one_sentence_serves_every_unavailable_cluster() {
    // The defect this pins: a message built from the cluster's name and status varies per
    // cluster and per transition, where AWS's is constant. So the sentence is collected
    // over every way a cluster can be unavailable and must come back as ONE value.
    let router = emulator();
    let mut messages = BTreeSet::new();
    for row in rows() {
        let cluster_arn = arrange(&router, &row).await;
        let (status, refused) = update(&router, &cluster_arn, row.patch.clone()).await;
        assert_eq!(
            status, 409,
            "case {:?}: an unavailable cluster refuses a modification with HTTP 409, got \
             {status} with {refused}",
            row.case
        );
        messages.insert(refused["message"].as_str().unwrap_or_default().to_string());
    }
    assert_eq!(
        messages,
        [IN_USE_SENTENCE.to_string()].into_iter().collect(),
        "case every unavailable cluster: one fixed sentence must serve them all, got {messages:?}"
    );

    // Positive control: a settled cluster still takes the very same modification, so the
    // refusal is about the cluster being in use and not about the request.
    let cluster_arn = create_cluster(&router, "fleetfoxtrot").await;
    wait_for_status(&router, &cluster_arn, "ACTIVE").await;
    let (status, accepted) = update(&router, &cluster_arn, json!({"shardCapacity": 8})).await;
    assert_eq!(
        status, 200,
        "case a settled cluster: the modification must be accepted, got {status} with {accepted}"
    );
    assert_eq!(
        accepted["cluster"]["shardCapacity"].as_i64(),
        Some(8),
        "case a settled cluster: the accepted modification must be applied, got {accepted}"
    );
}
