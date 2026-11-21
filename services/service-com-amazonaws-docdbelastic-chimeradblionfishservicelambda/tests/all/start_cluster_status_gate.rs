// SPDX-License-Identifier: CC0-1.0

//! Starting an elastic cluster is gated on the cluster's stored status.
//!
//! The rule these suites assert: **`StartCluster` acts only on a cluster whose stored
//! status is `STOPPED`** — it "restarts the stopped elastic cluster that is specified by
//! clusterARN" (`smithy_query(query="operation", operation="StartCluster")`) — **so a
//! cluster in any other status is refused with the `ValidationException` the operation
//! models (HTTP 400) and the refused call writes nothing, while a stopped cluster is
//! restarted (`STARTING`, then the available status the clock owes it).** The refusal is
//! stated as fixed text: the sentence interpolates no cluster name, ARN or status, so it
//! is byte-identical whichever non-stopped status the cluster is in and whatever shape it
//! has.
//!
//! The tables differ along what the rule generalises over: the status the cluster is in
//! when the start arrives (still being built, available, carrying a change in flight,
//! still stopping), the shape of the cluster (shard count, shard capacity, instances per
//! shard, auth type, tags), and how many times the refused start is retried.
//!
//! Each suite in this crate carries its own request harness (see
//! `all/cluster_lifecycle.rs`): the helpers are private to their module, so the few lines
//! below are the local copy rather than a second implementation of anything the service
//! itself owns.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;
use std::time::Duration;

/// The region every suite here signs with.
const REGION: &str = "us-east-2";

/// The sentence the service answers a start it will not act on with. Recorded behaviour:
/// the whole sentence is fixed text, which is exactly what the tables below assert by
/// comparing it across statuses and cluster shapes.
const NOT_STARTABLE: &str = "Can't start the cluster as it's currently in state unavailable for starting.";

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

/// `POST /cluster/{clusterArn}/start` — one `StartCluster` call.
async fn start(router: &MultiServiceRouter, cluster_arn: &str) -> (u16, Value) {
    let path = format!("/cluster/{}/start", label(cluster_arn));
    call(router, "POST", &path, None).await
}

/// `POST /cluster/{clusterArn}/stop` — one `StopCluster` call, asserted accepted.
async fn stop(router: &MultiServiceRouter, case: &str, cluster_arn: &str) {
    let path = format!("/cluster/{}/stop", label(cluster_arn));
    let (status, response) = call(router, "POST", &path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the setup stop must be accepted, got {status} with {response}"
    );
}

/// `GET /cluster/{clusterArn}` — the stored cluster, asserted readable.
async fn get_cluster(router: &MultiServiceRouter, case: &str, cluster_arn: &str) -> Value {
    let path = format!("/cluster/{}", label(cluster_arn));
    let (status, response) = call(router, "GET", &path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetCluster to answer 200, got {status} with {response}"
    );
    response["cluster"].clone()
}

/// A `CreateCluster` body with every required member, plus whatever `extra` carries.
fn create_body(name: &str, extra: &Value) -> Value {
    let mut body = json!({
        "clusterName": name,
        "authType": "PLAIN_TEXT",
        "adminUserName": "admin",
        "adminUserPassword": "Secret123!",
        "shardCapacity": 2,
        "shardCount": 1,
    });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        body[member] = value;
    }
    body
}

/// Create one cluster from `extra` and return its `clusterArn`, without waiting for it to
/// settle.
async fn create_cluster(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    extra: &Value,
) -> String {
    let (status, response) = call(router, "POST", "/cluster", Some(create_body(name, extra))).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateCluster to answer 200, got {status} with {response}"
    );
    response["cluster"]["clusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no clusterArn in {response}"))
        .to_string()
}

/// Poll `GetCluster` until the cluster reports `status` — the status the clock owes it a
/// short span after the call that started its current transition — and return it.
///
/// The deadline is deliberately generous (a minute of 50 ms looks): the suite waits for
/// the CONDITION, so a loaded host must not be able to exhaust it.
async fn wait_for_status(
    router: &MultiServiceRouter,
    case: &str,
    cluster_arn: &str,
    status: &str,
) -> Value {
    for _ in 0..1200 {
        let cluster = get_cluster(router, case, cluster_arn).await;
        if cluster["status"].as_str() == Some(status) {
            return cluster;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("case {case:?}: the cluster never settled to {status}");
}

/// Create one cluster of shape `extra` and return its `clusterArn` once it has settled to
/// the available status.
async fn create_active_cluster(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    extra: &Value,
) -> String {
    let cluster_arn = create_cluster(router, case, name, extra).await;
    wait_for_status(router, case, &cluster_arn, "ACTIVE").await;
    cluster_arn
}

/// Assert one `StartCluster` response is the refusal the operation models for a cluster it
/// may not act on, and return the message it carried.
fn assert_refused(case: &str, status: u16, refused: &Value) -> String {
    assert_eq!(
        status, 400,
        "case {case:?}: starting a cluster that is not stopped must answer HTTP 400, got \
         {status} with {refused}"
    );
    assert_eq!(
        error_code(refused),
        "ValidationException",
        "case {case:?}: expected a ValidationException, got {refused}"
    );
    refused["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no message in {refused}"))
        .to_string()
}

/// One cluster shape a start is refused from: the members `CreateCluster` carries beyond
/// the required minimum, and the name the cluster is created under.
struct Shape {
    case: &'static str,
    name: &'static str,
    extra: Value,
}

/// The shapes the tables below span: the minimum, several shards, the largest published
/// shard capacity, several instances per shard, and the other auth type with tags. The
/// bounds come from the `CreateCluster` input members (`smithy_query(query="operation",
/// operation="CreateCluster")`).
fn shapes() -> Vec<Shape> {
    vec![
        Shape {
            case: "the minimum shape: one shard, smallest capacity",
            name: "gateminimum",
            extra: json!({}),
        },
        Shape {
            case: "several shards",
            name: "gateshards",
            extra: json!({"shardCount": 4, "shardCapacity": 8}),
        },
        Shape {
            case: "the largest published shard capacity",
            name: "gatelargest",
            extra: json!({"shardCapacity": 64, "shardCount": 2}),
        },
        Shape {
            case: "multiple instances per shard",
            name: "gateinstances",
            extra: json!({"shardInstanceCount": 3, "shardCount": 3}),
        },
        Shape {
            case: "the other auth type, and tags",
            name: "gateauthtype",
            extra: json!({"authType": "SECRET_ARN", "tags": {"env": "test"}}),
        },
    ]
}

#[tokio::test]
async fn starting_an_available_cluster_is_refused_and_writes_nothing() {
    // The rows differ in the shape of the running cluster the start is addressed to, and
    // each row retries the refused start, so neither the answer nor the stored cluster can
    // be a function of how many times the call arrives.
    let router = emulator();
    for shape in &shapes() {
        let case = shape.case;
        let cluster_arn = create_active_cluster(&router, case, shape.name, &shape.extra).await;
        // The available status is a settled one, so the whole stored document is stable
        // across the refusals and can be compared member for member.
        let before = get_cluster(&router, case, &cluster_arn).await;

        for attempt in 1..=3 {
            let (status, refused) = start(&router, &cluster_arn).await;
            let message = assert_refused(case, status, &refused);
            assert_eq!(
                message, NOT_STARTABLE,
                "case {case:?} attempt {attempt}: expected the service's refusal sentence, got \
                 {message:?}"
            );
            // The sentence is fixed text: it names neither the cluster, nor its ARN, nor
            // the status it is in.
            for part in [
                shape.name,
                cluster_arn.as_str(),
                before["status"].as_str().unwrap_or("<no status>"),
            ] {
                assert!(
                    !message.contains(part),
                    "case {case:?} attempt {attempt}: the refusal interpolates nothing, but \
                     {message:?} carries {part:?}"
                );
            }
        }

        // The refused start wrote nothing: the cluster the next reader sees is the one that
        // was stored before the refusals, down to its shards.
        let after = get_cluster(&router, case, &cluster_arn).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused start must leave the stored cluster untouched, got \
             {after} where the stored cluster was {before}"
        );
        assert_eq!(
            after["status"].as_str(),
            Some("ACTIVE"),
            "case {case:?}: the running cluster must still be available, got {after}"
        );
    }
}

#[tokio::test]
async fn the_start_refusal_is_the_same_for_every_status_that_is_not_stopped() {
    // A start acts only from the stopped status, so the rows span the statuses it may not
    // act from: a cluster still being built, one that has settled available, one carrying a
    // change that is still in flight, and one that is still stopping. `allowed` is the
    // status the cluster may report after the refusal: the one it was in, or the one the
    // clock owed it whether or not anybody called.
    enum Setup {
        WhileCreating,
        WhenActive,
        WhileUpdating,
        WhileStopping,
    }

    let router = emulator();
    let cases: &[(&str, &str, Setup, &[&str])] = &[
        (
            "a cluster still being built",
            "notstartcreating",
            Setup::WhileCreating,
            &["CREATING", "ACTIVE"],
        ),
        (
            "an available cluster",
            "notstartactive",
            Setup::WhenActive,
            &["ACTIVE"],
        ),
        (
            "a cluster carrying a change in flight",
            "notstartupdating",
            Setup::WhileUpdating,
            &["UPDATING", "ACTIVE"],
        ),
        (
            "a cluster still stopping",
            "notstartstopping",
            Setup::WhileStopping,
            &["STOPPING", "STOPPED"],
        ),
    ];

    let mut sentences: Vec<(&str, String)> = Vec::new();
    for (case, name, setup, allowed) in cases {
        let extra = json!({"shardCount": 2});
        let cluster_arn = match setup {
            Setup::WhileCreating => create_cluster(&router, case, name, &extra).await,
            Setup::WhenActive => create_active_cluster(&router, case, name, &extra).await,
            Setup::WhileUpdating => {
                let arn = create_active_cluster(&router, case, name, &extra).await;
                let path = format!("/cluster/{}", label(&arn));
                let (status, response) =
                    call(&router, "PUT", &path, Some(json!({"shardCapacity": 8}))).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the setup modification must be accepted, got {status} with \
                     {response}"
                );
                arn
            }
            Setup::WhileStopping => {
                let arn = create_active_cluster(&router, case, name, &extra).await;
                stop(&router, case, &arn).await;
                arn
            }
        };

        let (status, answered) = start(&router, &cluster_arn).await;
        if status == 200 {
            // The one row whose setup status is transient on the way to STOPPED can have had
            // its stop become due before the start arrived, and a start acts from STOPPED: so
            // the accepted answer is the gate holding too, and it says the restart is in
            // flight. Nothing here waits on the clock to tell the two apart — the answer does.
            assert!(
                allowed.contains(&"STOPPED"),
                "case {case:?}: a start may only be accepted for a cluster that is stopped, \
                 but this row's cluster is never owed STOPPED; got {answered}"
            );
            assert_eq!(
                answered["cluster"]["status"].as_str(),
                Some("STARTING"),
                "case {case:?}: an accepted start reports the restart in flight, got {answered}"
            );
            continue;
        }
        let message = assert_refused(case, status, &answered);
        sentences.push((case, message));

        // The refusal moved nothing: the cluster reports the status it was already in, or
        // the one the clock owed it.
        let stored = get_cluster(&router, case, &cluster_arn).await;
        let reported = stored["status"].as_str().unwrap_or("<no status>");
        assert!(
            allowed.contains(&reported),
            "case {case:?}: a refused start must not move the status; expected one of \
             {allowed:?}, got {reported:?}"
        );
    }
    assert!(
        !sentences.is_empty(),
        "the table must refuse at least one start, or it asserts nothing"
    );

    for (case, message) in &sentences {
        assert_eq!(
            message, NOT_STARTABLE,
            "case {case:?}: every status that is not stopped is refused with the same fixed \
             sentence, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_stopped_cluster_starts_and_is_refused_again_once_it_is_running() {
    // The full gate, both ways round, on one cluster per row: refused while running,
    // accepted once stopped, refused again once the restart has settled. The rows differ in
    // the shape of the cluster, so the answers are not a property of one record.
    // The rows are named rather than taken by position, so reordering `shapes()` cannot
    // silently change which clusters this one drives.
    let round_trip = ["gateminimum", "gateshards"];
    let router = emulator();
    for shape in shapes().iter().filter(|shape| round_trip.contains(&shape.name)) {
        let case = shape.case;
        let name = format!("{}roundtrip", shape.name);
        let cluster_arn = create_active_cluster(&router, case, &name, &shape.extra).await;
        let running = get_cluster(&router, case, &cluster_arn).await;

        let (status, refused) = start(&router, &cluster_arn).await;
        let message = assert_refused(case, status, &refused);
        assert_eq!(
            message, NOT_STARTABLE,
            "case {case:?}: expected the service's refusal sentence, got {message:?}"
        );

        // Stopped is the one status a start acts from, so the stop is what opens the gate.
        stop(&router, case, &cluster_arn).await;
        let stopped = wait_for_status(&router, case, &cluster_arn, "STOPPED").await;

        let (status, accepted) = start(&router, &cluster_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: starting a stopped cluster must answer 200, got {status} with \
             {accepted}"
        );
        assert_eq!(
            accepted["cluster"]["status"].as_str(),
            Some("STARTING"),
            "case {case:?}: the restart is in flight when the call answers, got {accepted}"
        );
        for shard in accepted["cluster"]["shards"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no shards in {accepted}"))
        {
            assert_eq!(
                shard["status"].as_str(),
                Some("STARTING"),
                "case {case:?}: every shard moves with the cluster, got {shard}"
            );
        }

        // The restart is persisted, and it brought the stopped cluster back rather than
        // minting another one.
        let restarted = wait_for_status(&router, case, &cluster_arn, "ACTIVE").await;
        for member in [
            "clusterName",
            "clusterArn",
            "createTime",
            "clusterEndpoint",
            "adminUserName",
            "shardCapacity",
            "shardCount",
            "shardInstanceCount",
            "kmsKeyId",
        ] {
            assert_eq!(
                restarted.get(member),
                stopped.get(member),
                "case {case:?}: `{member}` must be the stopped cluster's, got {:?} where the \
                 stopped cluster has {:?}",
                restarted.get(member),
                stopped.get(member)
            );
        }
        // Stopping and restarting a cluster returns it to the cluster it was: the identity
        // and configuration the create persisted, and shards back in the running status.
        for member in [
            "authType",
            "subnetIds",
            "vpcSecurityGroupIds",
            "preferredMaintenanceWindow",
            "preferredBackupWindow",
            "backupRetentionPeriod",
        ] {
            assert_eq!(
                restarted.get(member),
                running.get(member),
                "case {case:?}: `{member}` must survive the stop and the restart, got {:?} \
                 where the running cluster had {:?}",
                restarted.get(member),
                running.get(member)
            );
        }
        assert_eq!(
            restarted["shards"], running["shards"],
            "case {case:?}: the restarted cluster's shards must be the running cluster's, got \
             {:?} where it had {:?}",
            restarted["shards"],
            running["shards"]
        );

        // And the gate closes behind it: the running cluster refuses a start once more.
        let (status, refused) = start(&router, &cluster_arn).await;
        let message = assert_refused(case, status, &refused);
        assert_eq!(
            message, NOT_STARTABLE,
            "case {case:?}: a restarted cluster refuses a further start with the same \
             sentence, got {message:?}"
        );
        let after = get_cluster(&router, case, &cluster_arn).await;
        assert_eq!(
            after, restarted,
            "case {case:?}: the second refusal wrote nothing either, got {after} where the \
             stored cluster was {restarted}"
        );
    }
}
