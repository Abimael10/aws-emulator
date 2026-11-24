// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the wire BODY of the `StopReplicationToReplica` refusal
//! a secret that is not a replica gets, driven over the multi-service router.
//!
//! The rule under test: `StopReplicationToReplica` is the replica-side promotion
//! call — it must be made "from the Region in which you want to promote the
//! replica to a primary secret" — so a `SecretId` that in the signed Region names
//! a secret which is NOT a replica is refused with HTTP 400,
//! `InvalidParameterException`, and the fixed text its replication state calls
//! for: a primary that still HAS a replica somewhere is told the Region the call
//! belongs in, and a secret with no replica at all is told that its class has no
//! promotion to perform. Each sentence is fixed text carrying no secret name, ARN
//! or Region from the request or from the store.
//!
//! The test is a TABLE over the class the rule generalises: whether the secret
//! has replicas and how many, the form of the `SecretId` (name, path-like name,
//! full ARN, partial ARN), the charset and size of the name, and whether a
//! replica is still `InProgress` or the clock has settled it to `InSync`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::state::{
    REPLICAS_PER_SECRET, REPLICA_STATUS_IN_SYNC,
};
use service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and so the Region every secret
/// it creates is homed in: the Region that holds the PRIMARY.
const REGION: &str = "us-west-2";

/// The fixed text `StopReplicationToReplica` refuses a primary that still has a
/// replica with: there IS a Region the call belongs in, and the sentence names
/// that class of secret and sends the caller there. Asserted as a literal rather
/// than read from the crate, so the test pins the text itself.
const REPLICATED_PRIMARY: &str =
    "Operation not permitted on a primary secret. Call must be made in replica secret's region.";

/// The fixed text it refuses a secret with no replica at all with — a standalone
/// secret, and one just promoted to standalone. No Region holds a replica of it,
/// so the sentence names the class alone and redirects nowhere.
const STANDALONE: &str = "Operation not permitted on a primary/standalone secret.";

/// How long the settled-replica test looks for the status it waits on before
/// giving up, and how often it looks. The ceiling only stops the suite from
/// hanging: what the wait looks for is the STATE the store reaches, never a span
/// passing.
const REPLICA_WAIT: Duration = Duration::from_secs(30);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// A SigV4 header naming `secretsmanager` in `region`; the router routes on the
/// credential scope and the emulator verifies signatures only when an
/// authenticator is registered, which this harness does not do.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/secretsmanager/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// A fresh emulator holding only this service, over its own engine.
fn emulator() -> MultiServiceRouter {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(EmulatedService::new(engine));
    router
}

/// Send one `awsJson1_1` request signed for [`REGION`], and return its status and
/// JSON body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization(REGION))
        .body(bytes::Bytes::from(
            // A write that stores a value creates a version the caller has to
            // name; see `secret_client_request_token::with_staked_version`.
            serde_json::to_vec(&crate::secret_client_request_token::with_staked_version(
                operation, body,
            ))
            .expect("the request body is serializable"),
        ))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Send one request that must succeed, and return its body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = call(router, operation, &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {operation}, got {status} with body {parsed}"
    );
    parsed
}

/// A required string member of a response body.
fn text(parsed: &Value, member: &str, case: &str) -> String {
    parsed
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no {member} in {parsed}"))
        .to_string()
}

/// How a row names the secret in its `SecretId`: the documented forms, "The ARN
/// or name of the secret" plus the partial ARN
/// (`GetSecretValueRequest$SecretId`, this service's Smithy model).
#[derive(Clone, Copy, Debug)]
enum IdForm {
    Name,
    FullArn,
    PartialArn,
}

/// The `SecretId` a row sends, built from the stored name and ARN.
fn secret_id(form: IdForm, name: &str, arn: &str) -> String {
    match form {
        IdForm::Name => name.to_string(),
        IdForm::FullArn => arn.to_string(),
        // The partial ARN is the full ARN without the trailing hyphen and the
        // six random characters the service appends to the secret name.
        IdForm::PartialArn => arn[..arn.len().saturating_sub(7)].to_string(),
    }
}

/// Call `StopReplicationToReplica` and assert the whole refusal: status, wire
/// error code and the fixed text. Asserting the message by EQUALITY is what pins
/// the "no variable part" half of the rule: a message carrying any secret name,
/// ARN or Region of the secret the request named would not be equal to this one.
async fn assert_refused(
    router: &MultiServiceRouter,
    sent_id: &str,
    expected: &str,
    case: &str,
) {
    let (status, parsed) =
        call(router, "StopReplicationToReplica", &json!({"SecretId": sent_id})).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {parsed}"
    );
    let raw = parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no __type in error body {parsed}"));
    let code = raw.split('#').next_back().unwrap_or(raw);
    assert_eq!(
        code, "InvalidParameterException",
        "case {case:?}: expected InvalidParameterException, got {code} in {parsed}"
    );
    let message = parsed
        .get("Message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no Message in error body {parsed}"));
    assert_eq!(
        message, expected,
        "case {case:?}: expected the fixed text {expected:?}, got {message:?}"
    );
}

#[tokio::test]
async fn stop_replication_to_replica_states_the_refusal_its_replication_state_calls_for() {
    // The rows vary what the rule generalises over: the number of replicas the
    // secret holds (none at all — a standalone secret — through several), which
    // of the two sentences that state calls for, the form of the `SecretId`, and
    // the charset and length of the name.
    let rows: &[(&str, &str, &[&str], IdForm, &str)] = &[
        (
            "standalone, by name",
            "standalone-secret",
            &[],
            IdForm::Name,
            STANDALONE,
        ),
        (
            "standalone with a path-like name, by name",
            "team/prod/db-password",
            &[],
            IdForm::Name,
            STANDALONE,
        ),
        (
            "standalone with a long mixed-charset name, by name",
            "Prod_Secret.With-Mixed+Chars=12345678901234567890",
            &[],
            IdForm::Name,
            STANDALONE,
        ),
        (
            "standalone, by full ARN",
            "standalone-by-arn",
            &[],
            IdForm::FullArn,
            STANDALONE,
        ),
        (
            "standalone, by partial ARN",
            "standalone-by-partial-arn",
            &[],
            IdForm::PartialArn,
            STANDALONE,
        ),
        (
            "primary with one replica, by name",
            "primary-one-replica",
            &["us-east-1"],
            IdForm::Name,
            REPLICATED_PRIMARY,
        ),
        (
            "primary with two replicas, by full ARN",
            "primary-two-replicas",
            &["us-east-1", "eu-west-1"],
            IdForm::FullArn,
            REPLICATED_PRIMARY,
        ),
        (
            "primary with several replicas, by partial ARN",
            "primary-many-replicas",
            &["us-east-1", "eu-west-1", "ap-northeast-1", "sa-east-1"],
            IdForm::PartialArn,
            REPLICATED_PRIMARY,
        ),
    ];

    for (case, name, replicas, form, expected) in rows {
        let router = emulator();
        let created = ok(
            &router,
            "CreateSecret",
            json!({"Name": name, "SecretString": "s3cret"}),
            case,
        )
        .await;
        let arn = text(&created, "ARN", case);
        let regions: Vec<String> = replicas.iter().map(|region| region.to_string()).collect();
        if !regions.is_empty() {
            ok(
                &router,
                "ReplicateSecretToRegions",
                json!({
                    "SecretId": name,
                    "AddReplicaRegions": regions
                        .iter()
                        .map(|region| json!({"Region": region}))
                        .collect::<Vec<Value>>(),
                }),
                case,
            )
            .await;
        }

        let sent = secret_id(*form, name, &arn);
        assert_refused(&router, &sent, expected, case).await;
    }

    // A count at the bound the store puts on one secret's replicas is the same
    // refusal: the sentence a replicated primary gets does not depend on how many
    // replicas it has.
    let router = emulator();
    let name = "primary-at-the-replica-bound";
    let case = "primary holding the maximum number of replicas";
    ok(
        &router,
        "CreateSecret",
        json!({"Name": name, "SecretString": "s3cret"}),
        case,
    )
    .await;
    let regions: Vec<String> = (0..REPLICAS_PER_SECRET)
        .map(|index| format!("emu-region-{index}"))
        .collect();
    ok(
        &router,
        "ReplicateSecretToRegions",
        json!({
            "SecretId": name,
            "AddReplicaRegions": regions
                .iter()
                .map(|region| json!({"Region": region}))
                .collect::<Vec<Value>>(),
        }),
        case,
    )
    .await;
    assert_refused(&router, name, REPLICATED_PRIMARY, case).await;
}

/// Look for the replica of `name` in the primary's own `ReplicationStatus` until
/// the store reports it `InSync`, reporting the statuses it holds: the suite waits
/// for that STATE rather than for a span.
async fn await_in_sync(router: &MultiServiceRouter, name: &str, case: &str) -> Vec<String> {
    let deadline = Instant::now() + REPLICA_WAIT;
    loop {
        let described = ok(router, "DescribeSecret", json!({"SecretId": name}), case).await;
        let statuses: Vec<String> = described
            .get("ReplicationStatus")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no ReplicationStatus in {described}"))
            .iter()
            .map(|entry| text(entry, "Status", case))
            .collect();
        if statuses
            .iter()
            .all(|status| status == REPLICA_STATUS_IN_SYNC)
        {
            return statuses;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the replica never settled, last statuses {statuses:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

#[tokio::test]
async fn the_refusal_is_the_same_sentence_once_the_replica_has_settled() {
    // A fresh replica is created `InProgress` and the clock moves it to
    // `InSync`; the primary's refusal does not depend on which of the two the
    // replica is in, so the sentence is asserted on both sides of that move.
    let router = emulator();
    let name = "primary-across-the-sync";
    let case = "primary whose only replica is still InProgress";
    ok(
        &router,
        "CreateSecret",
        json!({"Name": name, "SecretString": "s3cret"}),
        case,
    )
    .await;
    let replica_region = "us-east-1".to_string();
    ok(
        &router,
        "ReplicateSecretToRegions",
        json!({"SecretId": name, "AddReplicaRegions": [{"Region": replica_region}]}),
        case,
    )
    .await;
    assert_refused(&router, name, REPLICATED_PRIMARY, case).await;

    // Look for the move to `InSync` the store owes the replica until it has been
    // made, then assert the refusal again: a primary is told where the call
    // belongs whichever of the two states its replica is in.
    let settled = "primary whose replica has settled to InSync";
    let statuses = await_in_sync(&router, name, settled).await;
    assert_eq!(
        statuses,
        vec![REPLICA_STATUS_IN_SYNC.to_string()],
        "case {settled:?}: expected the one replica to have settled, got {statuses:?}"
    );
    assert_refused(&router, name, REPLICATED_PRIMARY, settled).await;
}
