// SPDX-License-Identifier: CC0-1.0

//! Which refusal `StopReplicationToReplica` answers a `SecretId` with that, in the Region
//! the request is signed for, names a secret that is NOT a replica.
//!
//! The rule under test: **the promotion is the replica-side call — it "[r]emoves the link
//! between the replica secret and the primary secret" and is made "from the Region in which
//! you want to promote the replica to a primary secret" (`StopReplicationToReplica`, the
//! Secrets Manager Smithy model) — so a `SecretId` that in the signed Region names a secret
//! with no link to remove is refused with HTTP 400 and `InvalidParameterException`, and the
//! sentence is read off that secret's replication state as it stands at the call and off
//! nothing else: a secret this service holds a replica of in some Region is told the class
//! of secret it is and the Region the call belongs in, and a secret no Region holds a
//! replica of is told the class of secret it is alone.** Each sentence is fixed text: it
//! interpolates no secret name, no ARN and no Region, so the same two strings answer every
//! secret (recorded behaviour — the model carries no prose for either refusal, while the 400
//! and the error shape are what it declares).
//!
//! A secret no Region holds a replica of is not only a never-replicated one: a replication
//! that FAILED creates no replica, `RemoveRegionsFromReplication` "deletes the replica", and
//! the promotion itself leaves two such secrets behind — the promoted one and the primary it
//! stopped tracking. Each of those is refused with the no-replica sentence, which is why the
//! rows below walk a secret through those states rather than only creating one.
//!
//! The rows differ along what the rule generalises over: how many replicas the secret holds
//! (none, one, several) and how it came to hold that many (replicated, failed, removed,
//! promoted away), whether the clock has settled a replica from `InProgress` to `InSync`,
//! the Region the refused call is signed for, and the form of the `SecretId` (name, full
//! ARN, partial ARN).
//!
//! This suite lives here, beside the other suites of the assembled emulator, because the
//! owning crate still roots its integration tests at a hand-declared `tests/all.rs`, which a
//! fix may not edit; the router's `tests/all/` picks a new suite up with nothing to declare.
//! Every call goes through a router with the owning service mounted on one shared engine,
//! exactly as the production router mounts it, so each assertion exercises the real wire path.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::state::REPLICA_STATUS_IN_PROGRESS;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every secret below is created in, and so the Region that holds the PRIMARY.
const PRIMARY_REGION: &str = "us-west-2";

/// The Region most rows replicate into.
const AWAY_REGION: &str = "us-east-1";

/// The fixed text a secret this service holds a replica of somewhere is refused with: there
/// IS a Region the promotion belongs in, so the sentence names the class of secret the call
/// was made on and sends the caller there.
const REPLICATED_PRIMARY: &str =
    "Operation not permitted on a primary secret. Call must be made in replica secret's region.";

/// The fixed text a secret no Region holds a replica of is refused with: there is no replica
/// to promote anywhere, so the sentence names the class of secret and redirects nowhere.
const NO_REPLICA: &str = "Operation not permitted on a primary/standalone secret.";

/// How long a wait below looks for the state it waits on before giving up, and how often it
/// looks. The ceiling only stops the suite from hanging: what each wait looks for is the
/// STATE the store reaches, never a span passing.
const SETTLE_CEILING: Duration = Duration::from_secs(30);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// A SigV4 header naming `secretsmanager` in `region`: the routing key this service claims
/// on, and the Region the request is made in.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/secretsmanager/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// A `ClientRequestToken` naming the one version a create stakes, unique per `seed` and
/// padded to the 32-character minimum of `ClientRequestTokenType`: "If you generate a raw
/// HTTP request to the Secrets Manager service endpoint, then you must generate a
/// ClientRequestToken and include it in the request."
/// (`CreateSecretRequest$ClientRequestToken`, that service's Smithy model.)
fn version_token(seed: &str) -> String {
    let mut token = format!("v-{seed}-");
    assert!(
        token.len() <= 32,
        "seed {seed:?} needs {} characters, more than the 32 a token has room for",
        token.len()
    );
    while token.len() < 32 {
        token.push('0');
    }
    token
}

/// A fresh emulator: one router with Secrets Manager mounted over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(engine),
    );
    router
}

/// Send one `awsJson1_1` request signed for `region` and return its status and JSON body.
async fn call_in(
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

/// Send one request signed for `region` that must succeed, and return its body.
async fn ok_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    let (status, parsed) = call_in(router, region, operation, &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {operation} in {region}, got {status} with body {parsed}"
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

/// The statuses the primary reports for its replicas, in the order it reports them.
fn replica_statuses(described: &Value, case: &str) -> Vec<String> {
    described
        .get("ReplicationStatus")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no ReplicationStatus in {described}"))
        .iter()
        .map(|entry| text(entry, "Status", case))
        .collect()
}

/// How a row names the secret in its `SecretId`: "The name of the secret or the replica ARN"
/// (`StopReplicationToReplicaRequest$SecretId`, the Secrets Manager Smithy model), plus the
/// partial ARN every `SecretId` accepts.
#[derive(Clone, Copy, Debug)]
enum IdForm {
    Name,
    FullArn,
    PartialArn,
}

/// The `SecretId` a row sends, built from the name and the ARN the Region it asks in holds
/// the secret under.
fn secret_id(form: IdForm, name: &str, arn: &str) -> String {
    match form {
        IdForm::Name => name.to_string(),
        IdForm::FullArn => arn.to_string(),
        // The partial ARN is the full ARN without the trailing hyphen and the six random
        // characters the service appends to the secret's name.
        IdForm::PartialArn => arn[..arn.len().saturating_sub(7)].to_string(),
    }
}

/// One row: the replication state it drives one secret into, where it then attempts the
/// promotion, and the fixed text that state calls for.
struct Row {
    case: &'static str,
    /// Regions that already hold a secret of this name when the replication is asked for, so
    /// the replica there is recorded `Failed` and no replica of this secret exists in them.
    occupied: &'static [&'static str],
    /// Regions the secret is replicated into.
    replicate: &'static [&'static str],
    /// Whether the row waits for the replicas the store owes a settled status before
    /// attempting the promotion.
    settle: bool,
    /// Regions removed again with `RemoveRegionsFromReplication`.
    remove: &'static [&'static str],
    /// The Region a replica is promoted in first, if the row promotes one.
    promote_in: Option<&'static str>,
    /// The Region the refused promotion is attempted in.
    ask_in: &'static str,
    /// How that attempt names the secret.
    form: IdForm,
    expected: &'static str,
}

/// The rows, in the order one secret reaches these states.
const ROWS: &[Row] = &[
    Row {
        case: "never replicated, by name",
        occupied: &[],
        replicate: &[],
        settle: false,
        remove: &[],
        promote_in: None,
        ask_in: PRIMARY_REGION,
        form: IdForm::Name,
        expected: NO_REPLICA,
    },
    Row {
        case: "never replicated, by full ARN",
        occupied: &[],
        replicate: &[],
        settle: false,
        remove: &[],
        promote_in: None,
        ask_in: PRIMARY_REGION,
        form: IdForm::FullArn,
        expected: NO_REPLICA,
    },
    Row {
        case: "a replication that failed, so no Region holds a replica",
        occupied: &[AWAY_REGION],
        replicate: &[AWAY_REGION],
        settle: true,
        remove: &[],
        promote_in: None,
        ask_in: PRIMARY_REGION,
        form: IdForm::PartialArn,
        expected: NO_REPLICA,
    },
    Row {
        case: "one replica, still InProgress, by name",
        occupied: &[],
        replicate: &[AWAY_REGION],
        settle: false,
        remove: &[],
        promote_in: None,
        ask_in: PRIMARY_REGION,
        form: IdForm::Name,
        expected: REPLICATED_PRIMARY,
    },
    Row {
        case: "one replica, settled InSync, by full ARN",
        occupied: &[],
        replicate: &[AWAY_REGION],
        settle: true,
        remove: &[],
        promote_in: None,
        ask_in: PRIMARY_REGION,
        form: IdForm::FullArn,
        expected: REPLICATED_PRIMARY,
    },
    Row {
        case: "three replicas, by partial ARN",
        occupied: &[],
        replicate: &[AWAY_REGION, "eu-west-1", "ap-northeast-1"],
        settle: true,
        remove: &[],
        promote_in: None,
        ask_in: PRIMARY_REGION,
        form: IdForm::PartialArn,
        expected: REPLICATED_PRIMARY,
    },
    Row {
        case: "a live replica beside one whose replication failed",
        occupied: &["eu-west-1"],
        replicate: &[AWAY_REGION, "eu-west-1"],
        settle: true,
        remove: &[],
        promote_in: None,
        ask_in: PRIMARY_REGION,
        form: IdForm::Name,
        expected: REPLICATED_PRIMARY,
    },
    Row {
        case: "one of two replicas removed again",
        occupied: &[],
        replicate: &[AWAY_REGION, "eu-west-1"],
        settle: true,
        remove: &["eu-west-1"],
        promote_in: None,
        ask_in: PRIMARY_REGION,
        form: IdForm::Name,
        expected: REPLICATED_PRIMARY,
    },
    Row {
        case: "every replica removed again",
        occupied: &[],
        replicate: &[AWAY_REGION, "eu-west-1"],
        settle: true,
        remove: &[AWAY_REGION, "eu-west-1"],
        promote_in: None,
        ask_in: PRIMARY_REGION,
        form: IdForm::Name,
        expected: NO_REPLICA,
    },
    Row {
        case: "the secret a promotion left in the replica's Region",
        occupied: &[],
        replicate: &[AWAY_REGION],
        settle: true,
        remove: &[],
        promote_in: Some(AWAY_REGION),
        ask_in: AWAY_REGION,
        form: IdForm::Name,
        expected: NO_REPLICA,
    },
    Row {
        case: "the primary whose only replica was promoted away",
        occupied: &[],
        replicate: &[AWAY_REGION],
        settle: true,
        remove: &[],
        promote_in: Some(AWAY_REGION),
        ask_in: PRIMARY_REGION,
        form: IdForm::FullArn,
        expected: NO_REPLICA,
    },
];

/// Look for the replicas of `name` the store still owes a settled status until it has
/// reported one for each: a row that asks for a settled replica asks for the STATE rather
/// than for a span. A replica recorded `Failed` has nothing left owed to it, so its entry
/// ends the wait too.
async fn await_settled(router: &MultiServiceRouter, name: &str, case: &str) {
    let deadline = Instant::now() + SETTLE_CEILING;
    loop {
        let described = ok_in(
            router,
            PRIMARY_REGION,
            "DescribeSecret",
            json!({"SecretId": name}),
            case,
        )
        .await;
        let statuses = replica_statuses(&described, case);
        if statuses
            .iter()
            .all(|status| status != REPLICA_STATUS_IN_PROGRESS)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: a replica never settled, last statuses {statuses:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Look for the replica of `name` in `region` until that Region answers for it: the secret a
/// promotion made there is the secret it promotes.
async fn await_replica(router: &MultiServiceRouter, region: &str, name: &str, case: &str) {
    let deadline = Instant::now() + SETTLE_CEILING;
    loop {
        let (status, parsed) =
            call_in(router, region, "DescribeSecret", &json!({"SecretId": name})).await;
        if status == 200 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the replica never appeared in {region}, last answer {status} with \
             {parsed}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

#[tokio::test]
async fn the_promotion_refusal_is_read_off_the_secrets_replication_state() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // One emulator per row, so the state a row drives its secret into is the only state
        // its refusal is decided against.
        let router = emulator();
        let name = format!("promotion-state-{index}");

        // A destination Region that already holds a secret of this name is what makes the
        // replication into it fail, and so what leaves the primary holding a replication
        // entry but no replica.
        for (occupant, region) in row.occupied.iter().enumerate() {
            ok_in(
                &router,
                region,
                "CreateSecret",
                json!({
                    "Name": &name,
                    "ClientRequestToken": version_token(&format!("occ{index}x{occupant}")),
                    "SecretString": "the destination's own secret",
                }),
                case,
            )
            .await;
        }
        ok_in(
            &router,
            PRIMARY_REGION,
            "CreateSecret",
            json!({
                "Name": &name,
                "ClientRequestToken": version_token(&format!("cre{index}")),
                "SecretString": "s3cret",
            }),
            case,
        )
        .await;
        if !row.replicate.is_empty() {
            ok_in(
                &router,
                PRIMARY_REGION,
                "ReplicateSecretToRegions",
                json!({
                    "SecretId": &name,
                    "AddReplicaRegions": row
                        .replicate
                        .iter()
                        .map(|region| json!({"Region": region}))
                        .collect::<Vec<Value>>(),
                }),
                case,
            )
            .await;
        }
        if row.settle {
            await_settled(&router, &name, case).await;
        }
        if !row.remove.is_empty() {
            ok_in(
                &router,
                PRIMARY_REGION,
                "RemoveRegionsFromReplication",
                json!({"SecretId": &name, "RemoveReplicaRegions": row.remove}),
                case,
            )
            .await;
        }
        if let Some(region) = row.promote_in {
            await_replica(&router, region, &name, case).await;
            let promoted = ok_in(
                &router,
                region,
                "StopReplicationToReplica",
                json!({"SecretId": &name}),
                case,
            )
            .await;
            assert!(
                promoted.get("ARN").and_then(Value::as_str).is_some(),
                "case {case:?}: expected the served promotion to answer the promoted ARN, got \
                 {promoted}"
            );
        }

        // The ARN the Region the attempt is made in holds the secret under, which the ARN
        // forms of the `SecretId` are built from.
        let described = ok_in(
            &router,
            row.ask_in,
            "DescribeSecret",
            json!({"SecretId": &name}),
            case,
        )
        .await;
        let arn = text(&described, "ARN", case);
        let sent = secret_id(row.form, &name, &arn);

        let (status, parsed) = call_in(
            &router,
            row.ask_in,
            "StopReplicationToReplica",
            &json!({"SecretId": sent}),
        )
        .await;
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
        // Asserting the message by EQUALITY is what pins the "no variable part" half of the
        // rule: a sentence carrying this secret's name, ARN or Region, or naming the other
        // class of secret, would not be equal to the fixed text.
        let message = parsed
            .get("Message")
            .or_else(|| parsed.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no message in error body {parsed}"));
        assert_eq!(
            message, row.expected,
            "case {case:?}: expected the fixed text {:?}, got {message:?}",
            row.expected
        );
    }
}

#[tokio::test]
async fn the_two_refusals_are_distinct_fixed_texts_answered_for_two_secrets_alike() {
    // What makes reading the sentence off the state meaningful: the two sentences differ, and
    // each is the SAME string for secrets that differ in name, in id form and in Region. Two
    // secrets per state, so a sentence built from the secret the request named could not be
    // equal for both.
    assert_ne!(
        REPLICATED_PRIMARY, NO_REPLICA,
        "the two refusals of this operation are distinct sentences"
    );

    let router = emulator();
    let cases: &[(&str, &str, bool)] = &[
        ("a short name", "pair-a", false),
        ("a path-like name", "team/prod/db-password", false),
        ("a long mixed-charset name", "Prod_Secret.With-Mixed+Chars=1234567890", true),
        ("a name of one character", "p", true),
    ];
    for (index, (case, name, replicated)) in cases.iter().enumerate() {
        ok_in(
            &router,
            PRIMARY_REGION,
            "CreateSecret",
            json!({
                "Name": name,
                "ClientRequestToken": version_token(&format!("pair{index}")),
                "SecretString": "s3cret",
            }),
            case,
        )
        .await;
        if *replicated {
            ok_in(
                &router,
                PRIMARY_REGION,
                "ReplicateSecretToRegions",
                json!({"SecretId": name, "AddReplicaRegions": [{"Region": AWAY_REGION}]}),
                case,
            )
            .await;
        }
        let expected = if *replicated {
            REPLICATED_PRIMARY
        } else {
            NO_REPLICA
        };
        let (status, parsed) = call_in(
            &router,
            PRIMARY_REGION,
            "StopReplicationToReplica",
            &json!({"SecretId": name}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {parsed}"
        );
        let message = parsed
            .get("Message")
            .or_else(|| parsed.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no message in error body {parsed}"));
        assert_eq!(
            message, expected,
            "case {case:?}: expected the fixed text {expected:?}, got {message:?}"
        );
    }
}
