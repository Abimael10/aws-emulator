// SPDX-License-Identifier: CC0-1.0

//! Where a refusal of this service states its message, and what the two halves of
//! its cross-Region refusal pair say.
//!
//! The rules these tables assert:
//!
//! * **One member name for every error body.** Every error shape this service's
//!   model declares names its message member `Message`
//!   (`InvalidParameterException$Message`, `InvalidRequestException$Message`,
//!   `ResourceNotFoundException$Message`, … — this service's Smithy model), and the
//!   `awsJson1_1` envelope writes a member under the name its shape gives it. So
//!   every refusal this service answers states its message under `Message` and
//!   under nothing else — the common `AccessDeniedException`, which no AWS service
//!   models and which this service therefore answers out of a shape of its own,
//!   included. A client reading the member this service documents has to find the
//!   message there.
//! * **A cross-Region refusal names the Region the call belongs in.** A replica
//!   tracks its primary, so a write is made in the primary's Region and the
//!   promotion `StopReplicationToReplica` performs is made "from the Region in
//!   which you want to promote the replica to a primary secret"
//!   (`StopReplicationToReplica`, this service's Smithy model). Each half of that
//!   pair is refused with one fixed text stating the class of secret the call is
//!   not permitted on AND the Region it has to be made in instead — and the same
//!   call made on the other side is served. A secret with no replica at all has no
//!   other Region to be named, so its refusal states its class alone.
//!
//! The rows of the first table span the error SHAPES a refusal belongs to (three
//! the model declares and the one it does not), the operations that raise them, and
//! whether the message is fixed text or built from the request; the second table
//! spans the two sides of one replicated secret and the operations refused on each.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region the primary of every secret below is created in, and the Region the
/// first table signs every request for.
const PRIMARY_REGION: &str = "us-west-2";

/// The Region the replicated secret of the second table is replicated into.
const REPLICA_REGION: &str = "us-east-1";

/// What `StopReplicationToReplica` answers for a `SecretId` that, in the signed
/// Region, names a secret which is not a replica but still HAS one. Pinned as a
/// literal rather than read from the crate, so the table states the text itself.
const NOT_A_REPLICA: &str =
    "Operation not permitted on a primary secret. Call must be made in replica secret's region.";

/// What it answers for one that is not a replica and has none either: no Region
/// holds a replica of it, so the sentence names its class and redirects nowhere.
const STANDALONE: &str = "Operation not permitted on a primary/standalone secret.";

/// What a write answers for a `SecretId` that, in the signed Region, names a
/// replica: the other half of the same rule, naming the other Region.
const NOT_PERMITTED_ON_REPLICA: &str =
    "Operation not permitted on a replica secret. Call must be made in primary secret's region.";

/// What an operation answers for a `SecretId` the signed Region holds no secret
/// under.
const SECRET_NOT_FOUND: &str = "Secrets Manager can't find the specified secret.";

/// What `DeleteSecret` answers for a `RecoveryWindowInDays` outside the range the
/// member documents: "The number of days from 7 to 30 that Secrets Manager waits
/// before permanently deleting the secret."
/// (`DeleteSecretRequest$RecoveryWindowInDays`, this service's Smithy model.)
const RECOVERY_WINDOW_OUT_OF_RANGE: &str =
    "The RecoveryWindowInDays value must be between 7 and 30 days (inclusive).";

/// What `RotateSecret` answers when neither the call nor the secret states a
/// rotation function.
const NO_ROTATION_FUNCTION: &str = "No Lambda rotation function ARN is associated with this secret.";

/// What `RotateSecret` answers for a rotation function this service may not invoke:
/// the function's own resource-based policy is what has to grant it, and a function
/// no Lambda holds grants nothing.
const INVOKE_NOT_PERMITTED: &str = "Secrets Manager cannot invoke the specified Lambda function. \
     Ensure that the function policy grants access to the principal secretsmanager.amazonaws.com.";

/// A Lambda function ARN no function is held under, which is what makes the
/// rotation above unauthorizable.
const ABSENT_FUNCTION: &str = "arn:aws:lambda:us-west-2:000000000000:function:no-such-rotator";

/// How long the second table looks for the replica it asked for before giving up,
/// and how often it looks. The ceiling only stops a suite from hanging: what each
/// wait below looks for is the STATE the store reaches, never a span passing.
const REPLICA_WAIT: Duration = Duration::from_secs(30);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// Secrets Manager and Lambda over ONE shared engine, the shape
/// `core/router/src/lib.rs` mounts them in: whether a rotation function may be
/// invoked is Lambda's own answer about that function.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A SigV4 header whose credential scope names `region`, which is what routes the
/// request and which Region it is made in.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/secretsmanager/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// POST one `awsJson1_1` Secrets Manager call signed for `region`, as its status
/// and parsed body.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization(region))
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body is serializable"),
        ))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The same, requiring the 200 the operation answers.
async fn ok(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
    case: &str,
) -> Value {
    let (status, parsed) = call(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {operation}, got {status} with body {parsed}"
    );
    parsed
}

/// The short name of the error shape an `awsJson1_1` error body discriminates by.
fn error_code(parsed: &Value, case: &str) -> String {
    let raw = parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no __type in error body {parsed}"));
    raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string()
}

/// Assert one whole refusal: the status, the error shape, and the message under the
/// member this service words it with — and under no other spelling of that member,
/// which is the half of the rule a client reading `Message` depends on.
async fn assert_refused(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
    expected_code: &str,
    expected_message: &str,
    case: &str,
) {
    let (status, parsed) = call(router, region, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from {operation}, got {status} with body {parsed}"
    );
    let code = error_code(&parsed, case);
    assert_eq!(
        code, expected_code,
        "case {case:?}: expected {expected_code}, got {code} in {parsed}"
    );
    let stated = parsed.get("Message").and_then(Value::as_str);
    assert_eq!(
        stated,
        Some(expected_message),
        "case {case:?}: expected the message under Message to be {expected_message:?}, got \
         {parsed}"
    );
    assert!(
        parsed.get("message").is_none(),
        "case {case:?}: expected the message under Message alone, got {parsed}"
    );
}

/// One refusal of the first table: which call is made, and what it has to be
/// refused with.
struct RefusalRow {
    case: &'static str,
    operation: &'static str,
    /// The request, built from the name of the secret the table created.
    body: fn(&str) -> Value,
    code: &'static str,
    /// The whole message, which is fixed text for every row.
    message: &'static str,
}

/// The rows: one per error SHAPE a refusal of this service belongs to, including the
/// common `AccessDeniedException` this service's model does not declare.
fn refusal_rows() -> Vec<RefusalRow> {
    vec![
        RefusalRow {
            case: "a secret this Region does not hold",
            operation: "DescribeSecret",
            body: |_| json!({"SecretId": "secret-no-region-holds"}),
            code: "ResourceNotFoundException",
            message: SECRET_NOT_FOUND,
        },
        RefusalRow {
            case: "the promotion call made on a standalone secret",
            operation: "StopReplicationToReplica",
            body: |name| json!({"SecretId": name}),
            code: "InvalidParameterException",
            message: STANDALONE,
        },
        RefusalRow {
            case: "a rotation of a secret that has no rotation function",
            operation: "RotateSecret",
            body: |name| json!({"SecretId": name}),
            code: "InvalidRequestException",
            message: NO_ROTATION_FUNCTION,
        },
        RefusalRow {
            case: "a rotation through a function this service may not invoke",
            operation: "RotateSecret",
            body: |name| json!({"SecretId": name, "RotationLambdaARN": ABSENT_FUNCTION}),
            code: "AccessDeniedException",
            message: INVOKE_NOT_PERMITTED,
        },
        RefusalRow {
            case: "a recovery window outside the range the member documents",
            operation: "DeleteSecret",
            body: |name| json!({"SecretId": name, "RecoveryWindowInDays": 3}),
            code: "InvalidParameterException",
            message: RECOVERY_WINDOW_OUT_OF_RANGE,
        },
    ]
}

#[tokio::test]
async fn every_refusal_states_its_message_under_the_modelled_member() {
    let router = emulator();
    for (index, row) in refusal_rows().iter().enumerate() {
        // One secret per row, so a row that writes cannot change what the next
        // row's refusal is decided against.
        let name = format!("refusal-row-{index}");
        ok(
            &router,
            PRIMARY_REGION,
            "CreateSecret",
            &json!({
                "Name": &name,
                "ClientRequestToken": format!("{:032}", index + 1),
                "SecretString": "the-stored-value",
            }),
            row.case,
        )
        .await;
        assert_refused(
            &router,
            PRIMARY_REGION,
            row.operation,
            &(row.body)(&name),
            row.code,
            row.message,
            row.case,
        )
        .await;
    }
}

/// Look for the replica the promotion table needs until the Region it was asked for
/// answers for it, and report the ARN that Region holds it under: the table waits
/// for the STATE rather than for a span.
async fn await_replica(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let deadline = Instant::now() + REPLICA_WAIT;
    loop {
        let (status, parsed) =
            call(router, REPLICA_REGION, "DescribeSecret", &json!({"SecretId": name})).await;
        if status == 200 {
            return parsed
                .get("ARN")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: no ARN in {parsed}"))
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the replica never appeared in {REPLICA_REGION}, last answer \
             {status} with {parsed}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

#[tokio::test]
async fn each_half_of_the_cross_region_pair_names_the_region_the_call_belongs_in() {
    let router = emulator();
    let case = "a secret with a primary and one replica";
    let name = "cross-region-pair";
    ok(
        &router,
        PRIMARY_REGION,
        "CreateSecret",
        &json!({
            "Name": name,
            "ClientRequestToken": format!("{:032}", 1),
            "SecretString": "the-stored-value",
        }),
        case,
    )
    .await;
    ok(
        &router,
        PRIMARY_REGION,
        "ReplicateSecretToRegions",
        &json!({"SecretId": name, "AddReplicaRegions": [{"Region": REPLICA_REGION}]}),
        case,
    )
    .await;
    let replica_arn = await_replica(&router, name, case).await;

    // The promotion made where the PRIMARY is states the primary class and the
    // Region the call belongs in.
    assert_refused(
        &router,
        PRIMARY_REGION,
        "StopReplicationToReplica",
        &json!({"SecretId": name}),
        "InvalidParameterException",
        NOT_A_REPLICA,
        "the promotion made in the primary's Region",
    )
    .await;

    // The writes made where the REPLICA is state the replica class and the other
    // Region. The rows differ along what the write changes: a value, the metadata,
    // and the rotation configuration.
    let replica_rows: &[(&str, &str, Value)] = &[
        (
            "a value written in the replica's Region",
            "PutSecretValue",
            json!({
                "SecretId": name,
                "ClientRequestToken": format!("{:032}", 2),
                "SecretString": "written-on-the-replica",
            }),
        ),
        (
            "metadata updated in the replica's Region",
            "UpdateSecret",
            json!({"SecretId": name, "Description": "updated on the replica"}),
        ),
        (
            "a rotation configured in the replica's Region",
            "RotateSecret",
            json!({"SecretId": name, "RotationLambdaARN": ABSENT_FUNCTION}),
        ),
    ];
    for (row_case, operation, body) in replica_rows {
        assert_refused(
            &router,
            REPLICA_REGION,
            operation,
            body,
            "InvalidParameterException",
            NOT_PERMITTED_ON_REPLICA,
            row_case,
        )
        .await;
    }

    // The same promotion call made on the side the rule names IS served, which is
    // what makes the two refusals above a redirection rather than a dead end.
    let promoted = ok(
        &router,
        REPLICA_REGION,
        "StopReplicationToReplica",
        &json!({"SecretId": name}),
        "the promotion made in the replica's Region",
    )
    .await;
    assert_eq!(
        promoted.get("ARN").and_then(Value::as_str),
        Some(replica_arn.as_str()),
        "case {case:?}: expected the promotion to answer for the replica, got {promoted}"
    );
}
