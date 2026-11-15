// SPDX-License-Identifier: CC0-1.0

//! WHO may perform a Kinesis action at all, driven end to end over the real wire path (an
//! awsJson1_1 request through the multi-service router into the mounted service), so what is
//! asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: every request to this service is authorized against the documents in
//! force on the principal whose credential signed it BEFORE the operation acts — the
//! identity-based policies of the IAM identity the credential names (the ROLE behind an
//! assumed-role session, which is what the credentials injected into a compute environment
//! running under an execution role present; the USER behind a long-term access key), beside
//! the resource's own policy — evaluated against `kinesis:<OperationName>` on the resource
//! the request ADDRESSES. A call no statement allows (IAM's default deny), or one an explicit
//! `Deny` covers, is answered HTTP 400 `AccessDeniedException` and changes NOTHING: a refused
//! `PutRecord` leaves the shard empty. A call a matching `Allow` covers reaches the
//! operation's own answer, and exactly the record it asked for lands.
//!
//! The role, the user, their policies and the session live in the IAM and STS stores of the
//! SAME shared engine — the stores those services' own handlers write — so the documents
//! under test are the ones `iam:PutRolePolicy` stores and the principal a refusal blames is
//! the one `sts:GetCallerIdentity` answers for the key the request signed with.
//!
//! The recorded flow this was derived from calls `PutRecord` with the credentials of a
//! function's execution role whose only statement allows the log actions, and is one row of
//! the tables below. The rows differ along what the rule generalises over: the OPERATION
//! (a write, two reads, a listing and a tagging call), the DOCUMENT in force (none at all,
//! another service's actions, another action of this service, the action scoped to another
//! stream, the action on this stream, the whole namespace, and an explicit deny beside an
//! allow of everything), the PRINCIPAL (an assumed-role session, an IAM user's long-term key,
//! the account's own published credential), and the IDENTIFIER the request addresses the
//! stream by (its name, its ARN).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    stream_arn, CONSUMER_ACTIVE, STREAM_ACTIVE,
};
use service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use smithy_server_runtime::MultiServiceRouter;

use crate::stream_tags::{error_code, error_message, poll_until, REGION};

/// The canonical emulator account id, spelled out so the test pins the ARN a client sees.
const ACCOUNT: &str = "000000000000";

/// The `X-Amz-Target` prefix of this service (`OPERATIONS.md`, "Wire protocol").
const TARGET_PREFIX: &str = "Kinesis_20131202";

/// The access key id of the account's own published credential: a key neither STS nor IAM
/// holds a record for, which is the position AWS account root credentials are in — and the
/// key every other suite of this crate signs with.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The access key id of the assumed-role session the role rows sign with.
const SESSION_KEY_ID: &str = "ASIAEXAMPLEKINESIS01";

/// The access key id of the IAM user's long-term key the user rows sign with.
const USER_KEY_ID: &str = "AKIAEXAMPLEKINESIS01";

/// The role the session is assumed from, and the session's own name — which is the name the
/// environment the credentials were injected into was opened for.
const ROLE: &str = "kinesis-writer";
const SESSION: &str = "record-forwarder";

/// The IAM user the long-term key belongs to.
const USER_NAME: &str = "kinesis-user";

/// The name of the inline policy the role and the user carry.
const POLICY_NAME: &str = "inline";

/// How long the seeded session lasts: an hour, STS's own default for a role session
/// (`AssumeRoleRequest$DurationSeconds`, "Default: 3600 seconds").
const SESSION_SECONDS: i32 = 3_600;

/// The error code an authorization refusal names, and the status it answers with: the shape
/// this model declares for it is a client error defaulted to HTTP 400
/// (`smithy_query(query="operation", operation="PutRecord")`).
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 400;

/// Kinesis, IAM and STS over ONE engine — the shape `core/router/src/lib.rs` mounts them in —
/// so the policies and the session the cases seed are the records those services hold.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The signature itself is not under test here, so requests are unauthenticated; the
        // router still routes by the SigV4 credential scope, which is where the request's
        // region and its access key id come from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// Call `operation` with `body`, signed with `access_key_id` — what names the principal
    /// the request speaks for.
    async fn call_as(&self, access_key_id: &str, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("kinesis.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/kinesis/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=deadbeef"
                ),
            )
            .body(Bytes::from(body.to_string()))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// Call `operation` as the account's own published credential, which this gate does not
    /// bound, asserting a 200 and answering the body.
    async fn ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, response) = self.call_as(ACCOUNT_KEY_ID, operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} to answer 200, got {status} with body \
             {response}"
        );
        response
    }
}

/// The principal ARN the seeded session speaks for — what a refusal must name.
fn session_arn() -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/{SESSION}")
}

/// The ARN the account holds the role under, which is the identity whose policies an
/// assumed-role session's request is judged on.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE}")
}

/// The IAM user's ARN, as IAM holds it and `sts:GetCallerIdentity` reports it.
fn user_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:user/{USER_NAME}")
}

/// Record the role and the assumed-role session [`SESSION_KEY_ID`] speaks for, as
/// `iam:CreateRole` and `sts:AssumeRole` record them. NO policy is attached.
fn given_session(engine: &Arc<engine::InMemoryEngine>) {
    let role_arn = role_arn();
    let iam_store = iam::service_state(&**engine).expect("the IAM store");
    iam_store.lock().roles.insert(
        iam::name_key(ROLE),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: ROLE.to_string(),
            role_id: "AROAEXAMPLEKINESISROLE".to_string(),
            arn: role_arn.clone(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
            assume_role_policy_document: None,
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    let vend = sts::NewSession {
        role_arn: Some(role_arn.clone()),
        ..sts::NewSession::new(
            sts::Identity {
                user_id: sts::role_id(&role_arn),
                account: ACCOUNT.to_string(),
                arn: session_arn(),
            },
            sts::SessionKind::AssumedRole,
            SESSION_SECONDS,
        )
    };
    let store = sts::service_state(&**engine).expect("the STS store");
    store
        .lock()
        .insert(vend.into_session(SESSION_KEY_ID, SystemTime::now()))
        .expect("the session is recorded");
}

/// Record the IAM user [`USER_KEY_ID`] belongs to, as `iam:CreateUser` and
/// `iam:CreateAccessKey` record one. NO policy is attached.
fn given_user_key(engine: &Arc<engine::InMemoryEngine>) {
    let minted = ::aws_smithy_types::DateTime::from_secs(0);
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.users.insert(
        iam::name_key(USER_NAME),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User {
            path: "/".to_string(),
            user_name: USER_NAME.to_string(),
            user_id: "AIDAEXAMPLEKINESISUSER".to_string(),
            arn: user_arn(),
            create_date: minted,
            password_last_used: None,
            permissions_boundary: None,
            tags: None,
        },
    );
    store.access_keys.insert(
        USER_KEY_ID.to_string(),
        iam::AccessKeyRecord {
            user_key: iam::name_key(USER_NAME),
            status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
            create_date: minted,
        },
    );
}

/// One inline policy holding `document`, as `iam:PutRolePolicy` / `iam:PutUserPolicy` store
/// one.
fn inline(document: &Value) -> BTreeMap<String, iam::InlinePolicy> {
    BTreeMap::from([(
        iam::name_key(POLICY_NAME),
        iam::InlinePolicy {
            policy_name: POLICY_NAME.to_string(),
            policy_document: document.to_string(),
        },
    )])
}

/// Attach `document` to the role as its inline policy, replacing whatever it carried. `None`
/// takes every policy off it, as `iam:DeleteRolePolicy` does.
fn given_role_policy(engine: &Arc<engine::InMemoryEngine>, document: Option<&Value>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.role_inline_policies.remove(&iam::name_key(ROLE));
    if let Some(document) = document {
        store
            .role_inline_policies
            .insert(iam::name_key(ROLE), inline(document));
    }
}

/// The same for the IAM user, as `iam:PutUserPolicy` / `iam:DeleteUserPolicy` do.
fn given_user_policy(engine: &Arc<engine::InMemoryEngine>, document: Option<&Value>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.user_inline_policies.remove(&iam::name_key(USER_NAME));
    if let Some(document) = document {
        store
            .user_inline_policies
            .insert(iam::name_key(USER_NAME), inline(document));
    }
}

/// A policy allowing `actions` on `resource`.
fn allow_on(actions: Value, resource: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resource }]
    })
}

/// Create the stream `name` with one shard and wait until it reports `ACTIVE`, through the
/// account's own credential — the setup every case starts from.
async fn given_stream(emulator: &Emulator, case: &str, name: &str) -> String {
    emulator
        .ok(
            case,
            "CreateStream",
            &json!({ "StreamName": name, "ShardCount": 1 }),
        )
        .await;
    poll_until(
        case,
        &format!("the stream {name} reports {STREAM_ACTIVE}"),
        || async {
            let (status, response) = emulator
                .call_as(
                    ACCOUNT_KEY_ID,
                    "DescribeStreamSummary",
                    &json!({ "StreamName": name }),
                )
                .await;
            status == 200
                && response["StreamDescriptionSummary"]["StreamStatus"].as_str()
                    == Some(STREAM_ACTIVE)
        },
    )
    .await;
    stream_arn(REGION, name)
}

/// A consumer registered with the stream `name`, past the transition its registration armed —
/// "a consumer can't read data while in the CREATING or DELETING states"
/// (`Consumer$ConsumerStatus`) — answering its `ConsumerARN`. Registered as the account's own
/// credential, which this gate does not bound.
async fn given_consumer(
    emulator: &Emulator,
    case: &str,
    stream: &str,
    consumer: &str,
) -> String {
    let registered = emulator
        .ok(
            case,
            "RegisterStreamConsumer",
            &json!({ "StreamARN": stream_arn(REGION, stream), "ConsumerName": consumer }),
        )
        .await;
    let arn = registered["Consumer"]["ConsumerARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a registration answers a ConsumerARN"))
        .to_string();
    poll_until(
        case,
        &format!("the consumer {consumer} reports {CONSUMER_ACTIVE}"),
        || async {
            let (status, response) = emulator
                .call_as(
                    ACCOUNT_KEY_ID,
                    "DescribeStreamConsumer",
                    &json!({ "ConsumerARN": arn }),
                )
                .await;
            status == 200
                && response["ConsumerDescription"]["ConsumerStatus"].as_str()
                    == Some(CONSUMER_ACTIVE)
        },
    )
    .await;
    arn
}

/// The id of the stream's one shard, read as the account's own credential.
async fn one_shard(emulator: &Emulator, case: &str, name: &str) -> String {
    let listed = emulator
        .ok(case, "ListShards", &json!({ "StreamName": name }))
        .await;
    listed["Shards"][0]["ShardId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the stream's one shard, got {listed}"))
        .to_string()
}

/// Every record the stream's one shard holds, read as the account's own credential.
async fn shard_records(emulator: &Emulator, case: &str, name: &str) -> Vec<Value> {
    let shard_id = one_shard(emulator, case, name).await;
    let iterator = emulator
        .ok(
            case,
            "GetShardIterator",
            &json!({
                "StreamName": name,
                "ShardId": shard_id,
                "ShardIteratorType": "TRIM_HORIZON",
            }),
        )
        .await;
    let read = emulator
        .ok(
            case,
            "GetRecords",
            &json!({ "ShardIterator": iterator["ShardIterator"] }),
        )
        .await;
    read["Records"].as_array().cloned().unwrap_or_default()
}

/// What the documents in force do with a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The call reaches the operation.
    Served,
    /// No readable statement allows it: IAM's default deny.
    DefaultDeny,
    /// An explicit `Deny` covers it.
    ExplicitDeny,
}

/// The refusal IAM words for a call the documents in force do not allow.
fn refusal(principal: &str, operation: &str, resource: &str, expect: Expect) -> String {
    let action = format!("kinesis:{operation}");
    let reason = match expect {
        Expect::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        _ => format!("because no identity-based policy allows the {action} action"),
    };
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")
}

/// The calls the tables below drive, as `(operation, the body addressing the stream `name`)`:
/// a write, two reads of the shard's position, a listing of the shards and a tagging call —
/// five of the operations this service serves, spanning its data plane and its control plane.
fn calls(name: &str, shard_id: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "PutRecord",
            json!({ "StreamName": name, "Data": "eA==", "PartitionKey": "pk1" }),
        ),
        (
            "GetShardIterator",
            json!({
                "StreamName": name,
                "ShardId": shard_id,
                "ShardIteratorType": "TRIM_HORIZON",
            }),
        ),
        ("DescribeStreamSummary", json!({ "StreamName": name })),
        ("ListShards", json!({ "StreamName": name })),
        (
            "AddTagsToStream",
            json!({ "StreamName": name, "Tags": { "env": "test" } }),
        ),
    ]
}

/// The credential a case signs with, and the principal it speaks for — what a refusal names.
struct Signer<'a> {
    access_key_id: &'a str,
    principal: &'a str,
}

/// Assert what the call `operation` answers the principal `signer` speaks for, given what the
/// documents in force say about it.
async fn assert_outcome(
    emulator: &Emulator,
    case: &str,
    signer: &Signer<'_>,
    operation: &str,
    body: &Value,
    resource: &str,
    expect: Expect,
) {
    let (status, response) = emulator
        .call_as(signer.access_key_id, operation, body)
        .await;
    if expect == Expect::Served {
        assert_eq!(
            status, 200,
            "case {case:?} on {operation}: expected the call to be served, got {status} with \
             {response}"
        );
        return;
    }
    assert_eq!(
        status, ACCESS_DENIED_STATUS,
        "case {case:?} on {operation}: expected HTTP {ACCESS_DENIED_STATUS}, got {status} with \
         {response}"
    );
    assert_eq!(
        error_code(&response),
        ACCESS_DENIED_EXCEPTION,
        "case {case:?} on {operation}: expected {ACCESS_DENIED_EXCEPTION}, got {response}"
    );
    assert_eq!(
        error_message(&response),
        refusal(signer.principal, operation, resource, expect),
        "case {case:?} on {operation}: expected IAM's wording, got {response}"
    );
}

/// RULE: every operation of this service is decided against `kinesis:<OperationName>` on the
/// resource the request addresses, judged on the documents in force on the ROLE behind the
/// session the request signed with.
#[tokio::test]
async fn the_documents_in_force_on_the_role_decide_every_call_of_the_session() {
    let case = "the documents in force on an assumed-role session";
    let emulator = Emulator::new();
    given_session(&emulator.engine);
    let name = "authorized-orders";
    let arn = given_stream(&emulator, case, name).await;
    let shard_id = one_shard(&emulator, case, name).await;
    let other = stream_arn(REGION, "authorized-archive");
    let principal = session_arn();
    let session = Signer {
        access_key_id: SESSION_KEY_ID,
        principal: &principal,
    };

    // Each row is a document in force on the role: what it says, and what it does with a
    // call. `None` is the role carrying no policy at all, which is the recorded case's role
    // as far as this service's actions go.
    let rows: Vec<(&str, Option<Value>, Expect)> = vec![
        ("no policy at all", None, Expect::DefaultDeny),
        (
            "another service's actions",
            Some(allow_on(json!("firehose:*"), json!("*"))),
            Expect::DefaultDeny,
        ),
        (
            "another action of this service",
            Some(allow_on(json!("kinesis:ListStreams"), json!("*"))),
            Expect::DefaultDeny,
        ),
        (
            "every action of this service, scoped to another stream",
            Some(allow_on(json!("kinesis:*"), json!(other.clone()))),
            Expect::DefaultDeny,
        ),
        (
            "every action of this service on this stream",
            Some(allow_on(json!("kinesis:*"), json!(arn.clone()))),
            Expect::Served,
        ),
        (
            "every action of this service on every resource",
            Some(allow_on(json!("kinesis:*"), json!("*"))),
            Expect::Served,
        ),
        (
            "an explicit deny beside an allow of everything",
            Some(json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": "*", "Resource": "*" },
                    { "Effect": "Deny", "Action": "kinesis:*", "Resource": "*" },
                ]
            })),
            Expect::ExplicitDeny,
        ),
    ];

    for (row, document, expect) in &rows {
        given_role_policy(&emulator.engine, document.as_ref());
        for (operation, body) in calls(name, &shard_id) {
            assert_outcome(&emulator, row, &session, operation, &body, &arn, *expect).await;
        }
    }

    // A grant of exactly the one action, on exactly this stream, serves that call and nothing
    // else of the surface: the decision is per ACTION, not per service.
    let only_put = allow_on(json!("kinesis:PutRecord"), json!(arn.clone()));
    given_role_policy(&emulator.engine, Some(&only_put));
    for (operation, body) in calls(name, &shard_id) {
        let expect = if operation == "PutRecord" {
            Expect::Served
        } else {
            Expect::DefaultDeny
        };
        assert_outcome(
            &emulator,
            "a grant of exactly one action of this service",
            &session,
            operation,
            &body,
            &arn,
            expect,
        )
        .await;
    }
}

/// RULE: a refused call has NO effect and a granted one has exactly the effect it asked for —
/// the recorded flow: the same `PutRecord` is refused while the role carries no grant for it
/// and served once the role is given one, and the shard holds exactly the record the served
/// call placed, under the partition key that call named.
#[tokio::test]
async fn a_refused_write_leaves_the_shard_empty_and_a_granted_one_lands_one_record() {
    let case = "a write refused, then granted";
    let emulator = Emulator::new();
    given_session(&emulator.engine);
    let name = "authorized-effects";
    let arn = given_stream(&emulator, case, name).await;
    // The role AWS gives a function by default: the log actions and nothing else.
    let logs_only = allow_on(
        json!(["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"]),
        json!("arn:aws:logs:*:*:*"),
    );
    given_role_policy(&emulator.engine, Some(&logs_only));
    let principal = session_arn();
    let session = Signer {
        access_key_id: SESSION_KEY_ID,
        principal: &principal,
    };

    // The rows differ along the partition key the write names, so the record that lands is
    // identified by what the request carried rather than by one recorded value.
    for key in ["denied", "pk-1", "a-much-longer-partition-key"] {
        let write = json!({ "StreamName": name, "Data": "eA==", "PartitionKey": key });
        assert_outcome(
            &emulator,
            &format!("no grant for the write, under the key {key:?}"),
            &session,
            "PutRecord",
            &write,
            &arn,
            Expect::DefaultDeny,
        )
        .await;
        let held = shard_records(&emulator, case, name).await;
        assert!(
            held.is_empty(),
            "case {case:?} under the key {key:?}: expected a refused write to leave the shard \
             empty, got {held:?}"
        );
    }

    // The SAME role is given the write, and the identical call is served.
    let granted = json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "kinesis:PutRecord", "Resource": arn },
        ]
    });
    given_role_policy(&emulator.engine, Some(&granted));
    let (status, placed) = emulator
        .call_as(
            SESSION_KEY_ID,
            "PutRecord",
            &json!({ "StreamName": name, "Data": "eA==", "PartitionKey": "granted" }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the granted write to be served, got {status} with {placed}"
    );
    let held = shard_records(&emulator, case, name).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected exactly one record on the shard, got {held:?}"
    );
    assert_eq!(
        held[0]["PartitionKey"].as_str(),
        Some("granted"),
        "case {case:?}: expected the record to carry the partition key the served call named, \
         got {held:?}"
    );
    assert_eq!(
        held[0]["SequenceNumber"].as_str(),
        placed["SequenceNumber"].as_str(),
        "case {case:?}: expected the stored record to be the one the write reported, got \
         {held:?} against {placed}"
    );
}

/// RULE: WHICH documents decide a call is read off the credential: an assumed-role session is
/// judged on the ROLE's documents and named by its session ARN, an IAM user's long-term key on
/// the USER's and named by the user ARN, and the account's own published credential — which
/// holds no IAM policies — is bounded by neither.
#[tokio::test]
async fn the_decision_is_taken_on_the_documents_of_the_principal_the_credential_names() {
    let case = "which principal the decision is taken on";
    let emulator = Emulator::new();
    given_session(&emulator.engine);
    given_user_key(&emulator.engine);
    let name = "authorized-principals";
    let arn = given_stream(&emulator, case, name).await;
    let write = json!({ "StreamName": name, "Data": "eA==", "PartitionKey": "pk1" });
    let grant = allow_on(json!("kinesis:PutRecord"), json!(arn.clone()));
    let session_principal = session_arn();
    let session = Signer {
        access_key_id: SESSION_KEY_ID,
        principal: &session_principal,
    };
    let user_principal = user_arn();
    let user = Signer {
        access_key_id: USER_KEY_ID,
        principal: &user_principal,
    };
    let account = Signer {
        access_key_id: ACCOUNT_KEY_ID,
        principal: ACCOUNT,
    };

    // Neither the role nor the user carries a document yet: both are refused, each named by
    // the ARN its own credential speaks for.
    for (row, signer) in [
        ("an assumed-role session with no policy", &session),
        ("an IAM user's long-term key with no policy", &user),
    ] {
        assert_outcome(
            &emulator,
            row,
            signer,
            "PutRecord",
            &write,
            &arn,
            Expect::DefaultDeny,
        )
        .await;
    }

    // A grant on the ROLE serves the session's call and says nothing about the user's, and
    // the other way round: the two principals carry their own documents.
    given_role_policy(&emulator.engine, Some(&grant));
    assert_outcome(
        &emulator,
        "the role granted the write",
        &session,
        "PutRecord",
        &write,
        &arn,
        Expect::Served,
    )
    .await;
    assert_outcome(
        &emulator,
        "the role granted the write, the user not",
        &user,
        "PutRecord",
        &write,
        &arn,
        Expect::DefaultDeny,
    )
    .await;
    given_role_policy(&emulator.engine, None);
    given_user_policy(&emulator.engine, Some(&grant));
    assert_outcome(
        &emulator,
        "the user granted the write",
        &user,
        "PutRecord",
        &write,
        &arn,
        Expect::Served,
    )
    .await;
    assert_outcome(
        &emulator,
        "the user granted the write, the role not",
        &session,
        "PutRecord",
        &write,
        &arn,
        Expect::DefaultDeny,
    )
    .await;

    // The account's own published credential holds no IAM policy — the position account root
    // credentials are in — and is served throughout.
    given_user_policy(&emulator.engine, None);
    assert_outcome(
        &emulator,
        "the account's own published credential",
        &account,
        "PutRecord",
        &write,
        &arn,
        Expect::Served,
    )
    .await;
}

/// RULE: the resource a call is judged against is the stream the request ADDRESSES, whichever
/// identifier it addressed it by — "you must use either the StreamARN or the StreamName
/// parameter, or both" (`PutRecord`) — so one grant scoped to the stream's ARN covers every
/// form of the request, and a grant scoped to another stream covers none of them.
#[tokio::test]
async fn the_resource_is_the_stream_the_request_addressed_whichever_identifier_it_used() {
    let case = "the identifier the request addresses the stream by";
    let emulator = Emulator::new();
    given_session(&emulator.engine);
    let name = "authorized-identifiers";
    let arn = given_stream(&emulator, case, name).await;
    let other_name = "authorized-identifiers-other";
    let other = given_stream(&emulator, case, other_name).await;
    let principal = session_arn();
    let session = Signer {
        access_key_id: SESSION_KEY_ID,
        principal: &principal,
    };

    // The forms a request addresses this stream by: the name alone, the ARN alone, and both.
    let forms: Vec<(&str, Value)> = vec![
        (
            "the stream name alone",
            json!({ "StreamName": name, "Data": "eA==", "PartitionKey": "pk1" }),
        ),
        (
            "the stream ARN alone",
            json!({ "StreamARN": arn, "Data": "eA==", "PartitionKey": "pk1" }),
        ),
        (
            "both identifiers",
            json!({
                "StreamName": name,
                "StreamARN": arn,
                "Data": "eA==",
                "PartitionKey": "pk1",
            }),
        ),
    ];

    // (case, the resource the grant is scoped to, whether it covers this stream)
    let grants: Vec<(&str, String, bool)> = vec![
        ("a grant scoped to this stream", arn.clone(), true),
        ("a grant scoped to another stream", other.clone(), false),
        (
            "a grant scoped to every stream of this account and Region",
            stream_arn(REGION, "*"),
            true,
        ),
    ];

    for (grant_case, resource, covered) in &grants {
        given_role_policy(
            &emulator.engine,
            Some(&allow_on(json!("kinesis:PutRecord"), json!(resource))),
        );
        for (form, body) in &forms {
            assert_outcome(
                &emulator,
                &format!("{grant_case}, addressed by {form}"),
                &session,
                "PutRecord",
                body,
                &arn,
                if *covered {
                    Expect::Served
                } else {
                    Expect::DefaultDeny
                },
            )
            .await;
        }
    }
}

/// RULE: the gate stands in front of the operation that answers an EVENT STREAM too, and the
/// resource it is decided against is the CONSUMER the request names — the resource type
/// `SubscribeToShard` addresses ("the consumer you specify in the ConsumerARN") — so a grant
/// scoped to the stream that holds the consumer does not cover it.
#[tokio::test]
async fn an_event_stream_subscription_is_decided_against_the_consumer_it_names() {
    let case = "a subscription to a registered consumer";
    let emulator = Emulator::new();
    given_session(&emulator.engine);
    let name = "authorized-fanout";
    let arn = given_stream(&emulator, case, name).await;
    let shard_id = one_shard(&emulator, case, name).await;
    let consumer = given_consumer(&emulator, case, name, "authorized-reader").await;
    let principal = session_arn();
    let session = Signer {
        access_key_id: SESSION_KEY_ID,
        principal: &principal,
    };
    let subscribe = json!({
        "ConsumerARN": consumer,
        "ShardId": shard_id,
        "StartingPosition": { "Type": "TRIM_HORIZON" },
    });

    // (case, the resource the grant is scoped to, what it does with the subscription)
    let rows: Vec<(&str, Option<String>, Expect)> = vec![
        ("no policy at all", None, Expect::DefaultDeny),
        (
            "a grant scoped to the stream that holds the consumer",
            Some(arn.clone()),
            Expect::DefaultDeny,
        ),
        (
            "a grant scoped to the consumer the request names",
            Some(consumer.clone()),
            Expect::Served,
        ),
    ];

    for (row, resource, expect) in &rows {
        let document = resource
            .as_ref()
            .map(|resource| allow_on(json!("kinesis:SubscribeToShard"), json!(resource)));
        given_role_policy(&emulator.engine, document.as_ref());
        assert_outcome(
            &emulator,
            row,
            &session,
            "SubscribeToShard",
            &subscribe,
            &consumer,
            *expect,
        )
        .await;
    }
}
