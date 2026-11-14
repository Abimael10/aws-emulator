// SPDX-License-Identifier: CC0-1.0

//! How `RestoreClusterFromSnapshot` WORDS a refusal of a cluster name already in use.
//!
//! The rule this suite asserts: a restore asked to mint a cluster under a name an
//! existing cluster already holds is refused 400 `ValidationException` (`smithy_query(
//! query="operation", operation="RestoreClusterFromSnapshot")`) whose `message` states
//! THIS operation's own sentence rather than the service's generic request-validation
//! form: fixed text stating the rule that was broken — cluster names are compared
//! case-insensitively — followed by the two names it compared, the one the REQUEST
//! spelled and the STORED name of the cluster already holding it. Those two names are
//! the only variable parts of the sentence.
//!
//! The table below differs along what the rule generalises over: the letter case the
//! request spells the taken name in (so the two names in the sentence are sometimes the
//! same and sometimes differ), the name's length (both published bounds of
//! `clusterName`), its character set, and whether the cluster holding the name is the
//! snapshot's own source cluster or another one.
//!
//! Each suite in this crate carries its own request harness (see
//! `all/cluster_lifecycle.rs`): the helpers below are the local copy rather than a
//! second implementation of anything the service itself owns.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// The region this suite signs with.
const REGION: &str = "eu-west-2";

/// The cluster the snapshot every row restores was taken from.
const SOURCE_NAME: &str = "restoresource";

/// The inclusive length bounds of a `clusterName`.
///
/// Source: `smithy_query(query="doc", shape="CreateClusterInput", member="clusterName")`
/// — "Must contain from 1 to 63 letters, numbers, or hyphens. The first character must
/// be a letter."
const CLUSTER_NAME_LEN: (usize, usize) = (1, 63);

/// The sentence this service answers when a restore asks for a name a cluster already
/// holds, with the two names it compares filled in.
///
/// Recorded AWS behavior: everything but the two names is fixed text. `requested` is the
/// name as the request spelled it; `existing` is the stored name of the holder.
fn name_taken_sentence(requested: &str, existing: &str) -> String {
    format!(
        "Invalid cluster name: Cluster names are case-insensitive and cannot be the same as name \
         as an existing cluster. Cluster name {requested} is not valid because cluster {existing} \
         exists."
    )
}

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

/// Create a cluster that must succeed, returning its `Cluster`.
async fn create_cluster(router: &MultiServiceRouter, name: &str) -> Value {
    let (status, response) = call(router, "POST", "/cluster", Some(minimal_create(name))).await;
    assert_eq!(
        status, 200,
        "case {name:?}: the fixture cluster must be created, got {status} with {response}"
    );
    response["cluster"].clone()
}

/// Snapshot `cluster_arn` under `name`, which must succeed, returning its ARN.
async fn create_snapshot(router: &MultiServiceRouter, cluster_arn: &str, name: &str) -> String {
    let (status, response) = call(
        router,
        "POST",
        "/cluster-snapshot",
        Some(json!({"clusterArn": cluster_arn, "snapshotName": name})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: the fixture snapshot must be created, got {status} with {response}"
    );
    response["snapshot"]["snapshotArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: no snapshotArn in {response}"))
        .to_string()
}

/// `POST /cluster-snapshot/{snapshotArn}/restore` — one `RestoreClusterFromSnapshot`.
async fn restore(
    router: &MultiServiceRouter,
    snapshot_arn: &str,
    cluster_name: &str,
) -> (u16, Value) {
    let path = format!("/cluster-snapshot/{}/restore", label(snapshot_arn));
    call(router, "POST", &path, Some(json!({"clusterName": cluster_name}))).await
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

/// Every cluster name `ListClusters` reports.
async fn listed_cluster_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, response) = call(router, "GET", "/clusters", None).await;
    assert_eq!(
        status, 200,
        "ListClusters must answer 200, got {status} with {response}"
    );
    response["clusters"]
        .as_array()
        .unwrap_or_else(|| panic!("ListClusters must carry `clusters`, got {response}"))
        .iter()
        .map(|cluster| {
            cluster["clusterName"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// One row: the name a cluster is created under (the snapshot's source cluster when it
/// is [`SOURCE_NAME`]) and the spelling the restore asks for.
struct Row {
    case: &'static str,
    created: String,
    requested: String,
}

/// The rows: the taken name spelled as stored and in other cases, at both published
/// length bounds, over different character sets, and held by the snapshot's own source
/// cluster as well as by another one.
fn rows() -> Vec<Row> {
    let (min, max) = CLUSTER_NAME_LEN;
    // The first character must be a letter, so the longest well-formed name is a letter
    // followed by `max - 1` more.
    let longest = format!("z{}", "y".repeat(max - 1));
    vec![
        Row {
            case: "the snapshot's own source cluster's name",
            created: SOURCE_NAME.to_string(),
            requested: SOURCE_NAME.to_string(),
        },
        Row {
            case: "the source cluster's name in upper case",
            created: SOURCE_NAME.to_string(),
            requested: SOURCE_NAME.to_ascii_uppercase(),
        },
        Row {
            case: "another cluster's name, as stored",
            created: "takenbravo".to_string(),
            requested: "takenbravo".to_string(),
        },
        Row {
            case: "another cluster's name in mixed case",
            created: "takencharlie".to_string(),
            requested: "TaKeNcHaRlIe".to_string(),
        },
        Row {
            case: "a name of letters, digits and hyphens",
            created: "fleet-07-shard-3".to_string(),
            requested: "fleet-07-shard-3".to_string(),
        },
        Row {
            case: "the shortest allowed name, asked for in upper case",
            created: "a".repeat(min),
            requested: "a".repeat(min).to_ascii_uppercase(),
        },
        Row {
            case: "the longest allowed name",
            created: longest.clone(),
            requested: longest,
        },
    ]
}

#[tokio::test]
async fn a_restore_under_a_taken_name_states_both_names_it_compared() {
    let router = emulator();
    let source = create_cluster(&router, SOURCE_NAME).await;
    let source_arn = source["clusterArn"]
        .as_str()
        .expect("the source cluster carries its ARN")
        .to_string();
    let snapshot_arn = create_snapshot(&router, &source_arn, "restoresource-snap").await;

    for row in rows() {
        let case = row.case;
        // The holder: the snapshot's own source cluster, or one created for this row.
        let holder_arn = if row.created == SOURCE_NAME {
            source_arn.clone()
        } else {
            create_cluster(&router, &row.created).await["clusterArn"]
                .as_str()
                .expect("the holder cluster carries its ARN")
                .to_string()
        };
        let holder_before = get_cluster(&router, &holder_arn).await;
        let names_before = listed_cluster_names(&router).await;

        let (status, refused) = restore(&router, &snapshot_arn, &row.requested).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a name already in use is refused as request validation (HTTP 400), \
             got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {refused}"
        );
        let message = refused["message"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the refusal must carry `message`, got {refused}")
            })
            .to_string();
        // A cluster name is stored lowercased, so the holder's stored name is the
        // requested name lowercased — which is the second name the sentence states.
        let existing = row.created.to_ascii_lowercase();
        assert_eq!(
            message,
            name_taken_sentence(&row.requested, &existing),
            "case {case:?}: expected this operation's own sentence, got {message:?}"
        );
        // The sentence is this operation's, not the service's generic form.
        assert!(
            !message.starts_with("Invalid Input - "),
            "case {case:?}: the refusal must not use the generic validation form, got {message:?}"
        );
        // Both names are named, as spelled: the request's and the holder's stored one.
        assert!(
            message.contains(&row.requested) && message.contains(&existing),
            "case {case:?}: the sentence must name both {:?} and {existing:?}, got {message:?}",
            row.requested
        );

        // The refusal is the whole effect: the holder is untouched and nothing new exists.
        let holder_after = get_cluster(&router, &holder_arn).await;
        for member in ["clusterArn", "clusterName", "shardCapacity", "shardCount"] {
            assert_eq!(
                holder_after[member], holder_before[member],
                "case {case:?}: the cluster holding the name must be untouched, but {member} \
                 changed from {:?} to {:?}",
                holder_before[member], holder_after[member]
            );
        }
        let names_after = listed_cluster_names(&router).await;
        assert_eq!(
            names_after, names_before,
            "case {case:?}: a refused restore must create no cluster, got {names_after:?}"
        );
    }
}

#[tokio::test]
async fn a_restore_under_a_free_name_still_mints_the_cluster() {
    // Positive control for the rule: the refusal is about the name being held, so a name
    // nobody holds is restored and reads back under its own ARN.
    let router = emulator();
    let source = create_cluster(&router, SOURCE_NAME).await;
    let source_arn = source["clusterArn"].as_str().expect("an ARN").to_string();
    let snapshot_arn = create_snapshot(&router, &source_arn, "freename-snap").await;

    // The rows differ in the spelling of a name no cluster holds, including one that
    // differs from a taken name only in more than case.
    for (case, requested) in [
        ("a plainly free name", "nobodyholdsthis"),
        ("a free name in mixed case", "NobodyHoldsThisEither"),
        ("a free name of letters, digits and hyphens", "free-07-shard-3"),
    ] {
        let (status, restored) = restore(&router, &snapshot_arn, requested).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a free name must still be restored, got {status} with {restored}"
        );
        let cluster_arn = restored["cluster"]["clusterArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no clusterArn in {restored}"))
            .to_string();
        let stored = get_cluster(&router, &cluster_arn).await;
        assert_eq!(
            stored["clusterName"].as_str(),
            Some(requested.to_ascii_lowercase().as_str()),
            "case {case:?}: the restored cluster must read back under its name, got {stored}"
        );

        // And the name is taken from then on: the same restore is now refused with the
        // sentence naming the name just minted.
        let (status, refused) = restore(&router, &snapshot_arn, requested).await;
        assert_eq!(
            status, 400,
            "case {case:?}: restoring under the name again must be refused, got {status} with \
             {refused}"
        );
        assert_eq!(
            refused["message"].as_str(),
            Some(
                name_taken_sentence(requested, &requested.to_ascii_lowercase())
                    .as_str()
            ),
            "case {case:?}: expected this operation's own sentence, got {refused}"
        );
    }
}
