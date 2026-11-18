// SPDX-License-Identifier: CC0-1.0

//! What a Kinesis stream's RESOURCE-BASED policy does to a request signed with a real
//! assumed-role session — driven end to end over the three services that decide it: IAM holds
//! the role and the identity-based document in force on it, STS mints the session the call is
//! signed with, and Kinesis resolves that credential, evaluates the document the stream
//! carries and either serves the call or refuses it.
//!
//! THE RULE UNDER TEST: an explicit `Deny` in the policy attached to the resource a request
//! ADDRESSES refuses that request whatever the principal's own identity-based documents allow,
//! for exactly the `(principal, action)` pairs the statement covers and no others; the refusal
//! is the modelled `AccessDeniedException` (HTTP 400) whose sentence names the principal the
//! credential speaks for, the action asked for, the resource addressed and the document that
//! withheld the grant (`with an explicit deny in a resource-based policy`); and the access it
//! withholds comes back as soon as the document is detached.
//!
//! Only a cross-service suite can pin this as a client meets it: the session has to be minted
//! by a real `sts:AssumeRole` of a role a real `iam:CreateRole` holds, under a document a real
//! `iam:PutRolePolicy` stored, and then read back through the answers Kinesis gives requests
//! signed with the credential STS handed out. The crate-local suites of
//! `services/service-com-amazonaws-kinesis-kinesis-20131202` seed those two stores directly;
//! nothing there proves the key STS MINTS resolves to the role whose documents decide the call.
//!
//! The recorded flow this was derived from attaches ONE `Deny` of `kinesis:GetRecords` naming
//! one assumed role to one stream, reads as that role, and detaches it again — one row of the
//! table below. The rows differ along what the rule generalises over: WHICH actions the
//! statement covers (exactly the one the recording named, two of them, the single-record write,
//! the batch write, the whole namespace, an action none of the calls makes), and the rows are
//! each checked over every call the session makes — a single-record write, a batch write, both
//! reads of the shard, a read that addresses its stream only through the iterator it was handed
//! and one that names the ARN as well, a shard listing and a stream read.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The form encoder every awsQuery suite of this crate shares, and the reader that pulls one
// element's text out of an XML answer: written ONCE, in the sibling that owns them.
use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for, and the one every ARN here names.
const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefix of the Kinesis service.
const KINESIS_TARGET_PREFIX: &str = "Kinesis_20131202";

/// The account's own published long-term key: the stream's owner, which no IAM document
/// bounds. The fixture is built with it, and it is the positive control every row keeps.
const AMBIENT_KEY: &str = "AKIDEXAMPLE";

/// `iam`'s and `sts`' own awsQuery API versions, the `Version` each call carries.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The role the session is a session of, the name of its inline document, and the name the
/// session is opened under.
const ROLE: &str = "kinesis-record-reader";
const ROLE_POLICY_NAME: &str = "AllowKinesisRead";
const SESSION_NAME: &str = "record-forwarder";

/// The error an authorization refusal names and the status it answers with: the shape this
/// model declares for it is a client error defaulted to HTTP 400
/// (`smithy_query(query="operation", operation="GetRecords", model_path="services/service-com-amazonaws-kinesis-kinesis-20131202/smithy-model/com-amazonaws-kinesis-kinesis-20131202.json")`).
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 400;

/// The reason clause a refusal carries when an explicit `Deny` in the RESOURCE's own policy
/// covers the call. Written out HERE rather than read from a crate: the wording is the
/// requirement, so a drift in what the service answers fails this suite instead of travelling
/// with it.
const RESOURCE_POLICY_EXPLICIT_DENY: &str = "with an explicit deny in a resource-based policy";

/// The payload the stream's owner writes, which every read of the shard must find.
const OWNER_RECORD: &str = "owner-visible-record";

/// The payload a row's served `PutRecord` writes.
const SESSION_RECORD: &str = "session-written-record";

/// How long a case keeps LOOKING for a condition the emulator reaches on its own clock — a
/// stream that reports `ACTIVE`. Far longer than any span the emulator chose, so a loaded host
/// only makes a case look again; no case measures a duration.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How long a case waits between two looks.
const BETWEEN_LOOKS: Duration = Duration::from_millis(50);

/// The three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These cases exercise authorization against stored documents, not request signing.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — what names the principal the request speaks for. The
/// signature is never checked: this router runs with authentication off.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// One Kinesis awsJson1_1 call signed with `access_key_id`, as `(status, body)`.
async fn kinesis_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("kinesis.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("{KINESIS_TARGET_PREFIX}.{operation}"),
        )
        .header("authorization", authorization(access_key_id, "kinesis"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed awsJson1_1 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Kinesis call made as the stream's owner, which must be served.
async fn owner_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = kinesis_as(router, AMBIENT_KEY, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} as the stream's owner must answer 200, got {status} \
         {answered}"
    );
    answered
}

/// One awsQuery form call to `signing_name`, signed with the account's own credential,
/// as `(status, the XML text the wire carried)`.
async fn query(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(AMBIENT_KEY, signing_name))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The `__type` an awsJson1_1 error body carries, reduced to the error's shape name.
fn error_code(body: &Value) -> String {
    let raw = body.get("__type").and_then(Value::as_str).unwrap_or_default();
    raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string()
}

/// The `message` an error body carries, under either casing the protocols use.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// IAM's refusal sentence: who was refused, which action, on which resource, and which
/// document withheld the grant. Written out HERE rather than read from a crate, for the reason
/// [`RESOURCE_POLICY_EXPLICIT_DENY`] records.
fn refusal(principal: &str, operation: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: kinesis:{operation} on resource: \
         {resource} {RESOURCE_POLICY_EXPLICIT_DENY}"
    )
}

/// The ARN this account holds the role under — the identity an assumed-role session's request
/// is judged on, and the principal a resource statement names it by.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/{ROLE}",
        ::engine::emulator_account_id()
    )
}

/// The ARN Kinesis keys a stream of this account and Region by, as its own model states one
/// (`^arn:aws.*:kinesis:.*:\d{12}:.*(stream|channel)/\S+$`, `PutResourcePolicyInput$ResourceARN`).
fn stream_arn(name: &str) -> String {
    format!(
        "arn:aws:kinesis:{REGION}:{}:stream/{name}",
        ::engine::emulator_account_id()
    )
}

/// A role trust policy admitting this account's own principals to `sts:AssumeRole`.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {
                "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()),
            },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The identity-based document in force on the role: every action the cases call, on every
/// resource — so what refuses a call can only be the stream's own policy.
fn identity_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "kinesis:GetRecords",
                "kinesis:GetShardIterator",
                "kinesis:ListShards",
                "kinesis:DescribeStreamSummary",
                "kinesis:PutRecord",
                "kinesis:PutRecords",
            ],
            "Resource": "*",
        }],
    })
}

/// A resource-based policy document denying `actions` to the role on `arn` — the shape this
/// member documents ("It must include the identity of the principal and the actions allowed on
/// this resource", `PutResourcePolicyInput$Policy`).
fn denying(actions: Value, arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "DenyRoleActions",
            "Effect": "Deny",
            "Principal": { "AWS": [role_arn()] },
            "Action": actions,
            "Resource": arn,
        }],
    })
    .to_string()
}

/// The credential one session was minted under: the access key id a later request signs with,
/// and the ARN STS reports for it — the principal a refusal blames.
struct Session {
    access_key_id: String,
    arn: String,
}

/// Hold the role through IAM's own `CreateRole`, put [`identity_document`] in force on it
/// through IAM's own `PutRolePolicy`, and mint a session of it through a real
/// `sts:AssumeRole`.
async fn given_session(router: &MultiServiceRouter) -> Session {
    let (status, created) = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &AssumeRolePolicyDocument={}",
            form_encode(&trust_policy().to_string())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateRole({ROLE:?}): expected 200, got {status} {created}"
    );
    assert_eq!(
        element(&created, "Arn"),
        role_arn(),
        "setup CreateRole({ROLE:?}): the role must be held under its own ARN, got {created}"
    );

    let (status, stored) = query(
        router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &PolicyName={ROLE_POLICY_NAME}&PolicyDocument={}",
            form_encode(&identity_document().to_string())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup PutRolePolicy({ROLE:?}): expected 200, got {status} {stored}"
    );

    let (status, assumed) = query(
        router,
        "sts",
        format!(
            "Action=AssumeRole&Version={STS_API_VERSION}&RoleArn={}&RoleSessionName={SESSION_NAME}",
            form_encode(&role_arn())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup AssumeRole({ROLE:?}): expected 200, got {status} {assumed}"
    );
    let session = Session {
        access_key_id: element(&assumed, "AccessKeyId"),
        arn: element(&assumed, "Arn"),
    };
    assert_eq!(
        session.arn,
        format!(
            "arn:aws:sts::{}:assumed-role/{ROLE}/{SESSION_NAME}",
            ::engine::emulator_account_id()
        ),
        "setup AssumeRole({ROLE:?}): the session must speak for the role it was assumed from, \
         got {assumed}"
    );
    session
}

/// A provisioned stream of one shard holding the owner's record, past the span its create
/// armed. Answers `(the stream's ARN, its one shard id)`.
async fn given_stream(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    owner_ok(
        router,
        case,
        "CreateStream",
        &json!({
            "StreamName": name,
            "ShardCount": 1,
            "StreamModeDetails": { "StreamMode": "PROVISIONED" },
        }),
    )
    .await;
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, answered) = kinesis_as(
            router,
            AMBIENT_KEY,
            "DescribeStreamSummary",
            &json!({ "StreamName": name }),
        )
        .await;
        let reported = answered
            .pointer("/StreamDescriptionSummary/StreamStatus")
            .and_then(Value::as_str);
        if status == 200 && reported == Some("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stream {name} to report ACTIVE, got {status} {answered}"
        );
        tokio::time::sleep(BETWEEN_LOOKS).await;
    }
    let arn = stream_arn(name);
    let listed = owner_ok(router, case, "ListShards", &json!({ "StreamName": name })).await;
    let shard_id = listed["Shards"][0]["ShardId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the stream's one shard, got {listed}"))
        .to_string();
    owner_ok(
        router,
        case,
        "PutRecord",
        &json!({
            "StreamARN": arn,
            "Data": ::aws_smithy_types::base64::encode(OWNER_RECORD.as_bytes()),
            "PartitionKey": "pk",
        }),
    )
    .await;
    (arn, shard_id)
}

/// A fresh iterator over the stream's one shard, taken as the owner: a `GetRecords` names its
/// stream only inside the iterator it was handed, and an iterator is read from once.
async fn fresh_iterator(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    shard_id: &str,
) -> String {
    let handed = owner_ok(
        router,
        case,
        "GetShardIterator",
        &json!({
            "StreamName": name,
            "ShardId": shard_id,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
    )
    .await;
    handed["ShardIterator"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a ShardIterator, got {handed}"))
        .to_string()
}

/// Every payload a `GetRecords` answer carries, decoded out of the base64 a blob member
/// travels as in an awsJson body.
fn payloads_of(case: &str, answered: &Value) -> Vec<String> {
    answered["Records"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Records list, got {answered}"))
        .iter()
        .map(|record| {
            let data = record["Data"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a record carried no Data: {answered}"));
            let bytes = ::aws_smithy_types::base64::decode(data)
                .unwrap_or_else(|_| panic!("case {case:?}: Data must be base64, got {data:?}"));
            String::from_utf8_lossy(&bytes).to_string()
        })
        .collect()
}

/// One call a row is checked over: the label the case names it by, the operation it invokes,
/// and its body.
struct Call {
    label: &'static str,
    operation: &'static str,
    body: Value,
}

/// Each call a row is checked over. Every `GetRecords` takes an iterator of its own, because a
/// read consumes the position the iterator names.
async fn calls(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    arn: &str,
    shard_id: &str,
) -> Vec<Call> {
    let by_iterator = fresh_iterator(router, case, name, shard_id).await;
    let by_arn = fresh_iterator(router, case, name, shard_id).await;
    vec![
        Call {
            label: "PutRecord",
            operation: "PutRecord",
            body: json!({
                "StreamName": name,
                "Data": ::aws_smithy_types::base64::encode(SESSION_RECORD.as_bytes()),
                "PartitionKey": "pk-session",
            }),
        },
        Call {
            label: "PutRecords",
            operation: "PutRecords",
            body: json!({
                "StreamName": name,
                "Records": [{
                    "Data": ::aws_smithy_types::base64::encode(SESSION_RECORD.as_bytes()),
                    "PartitionKey": "pk-session-batch",
                }],
            }),
        },
        Call {
            label: "GetShardIterator",
            operation: "GetShardIterator",
            body: json!({
                "StreamName": name,
                "ShardId": shard_id,
                "ShardIteratorType": "TRIM_HORIZON",
            }),
        },
        Call {
            label: "GetRecords addressing its stream through the iterator alone",
            operation: "GetRecords",
            body: json!({ "ShardIterator": by_iterator }),
        },
        Call {
            label: "GetRecords naming its stream's ARN as well",
            operation: "GetRecords",
            body: json!({ "ShardIterator": by_arn, "StreamARN": arn }),
        },
        Call {
            label: "ListShards",
            operation: "ListShards",
            body: json!({ "StreamName": name }),
        },
        Call {
            label: "DescribeStreamSummary",
            operation: "DescribeStreamSummary",
            body: json!({ "StreamName": name }),
        },
    ]
}

/// Assert what the session's call answers: served, or refused on the stream's own document with
/// IAM's sentence for that clause.
async fn assert_outcome(
    router: &MultiServiceRouter,
    case: &str,
    session: &Session,
    call: &Call,
    arn: &str,
    denied: bool,
) {
    let Call {
        label,
        operation,
        body,
    } = call;
    let (status, answered) = kinesis_as(router, &session.access_key_id, operation, body).await;
    if !denied {
        assert_eq!(
            status, 200,
            "case {case:?} on {label:?}: a call the document does not cover must be served, got \
             {status} with {answered}"
        );
        return;
    }
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED_EXCEPTION),
        "case {case:?} on {label:?}: expected the modelled HTTP {ACCESS_DENIED_STATUS} \
         {ACCESS_DENIED_EXCEPTION}, got {status} with {answered}"
    );
    assert_eq!(
        error_message(&answered),
        refusal(&session.arn, operation, arn),
        "case {case:?} on {label:?}: the refusal must name the principal STS minted, the action \
         asked for, the stream addressed and the document that denied it, got {answered}"
    );
}

/// Attach `policy` to `arn` as the stream's owner.
async fn attach(router: &MultiServiceRouter, case: &str, arn: &str, policy: &str) {
    owner_ok(
        router,
        case,
        "PutResourcePolicy",
        &json!({ "ResourceARN": arn, "Policy": policy }),
    )
    .await;
}

/// RULE: an explicit `Deny` in the stream's own policy refuses exactly the actions it names to
/// the principal it names, whatever that principal's identity-based documents allow; every
/// other call of the same session is served, and the stream's owner — whom the statement does
/// not name — keeps reading throughout.
#[tokio::test]
async fn a_resource_policy_deny_refuses_exactly_the_actions_it_names() {
    let case = "which actions the deny covers";
    let router = emulator();
    let name = "resource-policy-denied-reads";
    let (arn, shard_id) = given_stream(&router, case, name).await;
    let session = given_session(&router).await;

    // (case, the statement's `Action` element, the labels of the calls it covers)
    let rows: Vec<(&str, Value, &[&str])> = vec![
        (
            "the one read the recorded document denies",
            json!("kinesis:GetRecords"),
            &[
                "GetRecords addressing its stream through the iterator alone",
                "GetRecords naming its stream's ARN as well",
            ],
        ),
        (
            "both reads of the shard",
            json!(["kinesis:GetRecords", "kinesis:GetShardIterator"]),
            &[
                "GetShardIterator",
                "GetRecords addressing its stream through the iterator alone",
                "GetRecords naming its stream's ARN as well",
            ],
        ),
        (
            // A statement naming one write does not cover the batch write, whose own action
            // name it does not name.
            "the single-record write alone",
            json!("kinesis:PutRecord"),
            &["PutRecord"],
        ),
        (
            "the batch write alone",
            json!("kinesis:PutRecords"),
            &["PutRecords"],
        ),
        (
            "every action of this service",
            json!("kinesis:*"),
            &[
                "PutRecord",
                "PutRecords",
                "GetShardIterator",
                "GetRecords addressing its stream through the iterator alone",
                "GetRecords naming its stream's ARN as well",
                "ListShards",
                "DescribeStreamSummary",
            ],
        ),
        (
            "an action none of these calls makes",
            json!("kinesis:DeleteStream"),
            &[],
        ),
    ];

    for (row, actions, denied) in &rows {
        attach(&router, row, &arn, &denying(actions.clone(), &arn)).await;
        for call in &calls(&router, row, name, &arn, &shard_id).await {
            assert_outcome(&router, row, &session, call, &arn, denied.contains(&call.label)).await;
        }
        // The statement names one principal, so the stream's owner reads its own records
        // whatever it withholds from the session.
        let iterator = fresh_iterator(&router, row, name, &shard_id).await;
        let read = owner_ok(
            &router,
            row,
            "GetRecords",
            &json!({ "ShardIterator": iterator }),
        )
        .await;
        let found = payloads_of(row, &read);
        assert!(
            found.iter().any(|payload| payload == OWNER_RECORD),
            "case {row:?}: expected the owner to keep reading {OWNER_RECORD:?}, got {found:?}"
        );
    }
}

/// RULE: the call is decided on the document the resource carries AT THE TIME of the call, so a
/// `Deny` withholds the action from the moment it is attached and detaching the document gives
/// the action back — the same request then reading the records the stream holds.
#[tokio::test]
async fn detaching_the_document_gives_back_the_read_it_withheld() {
    let case = "attaching and detaching the document";
    let router = emulator();
    let name = "resource-policy-lifecycle-reads";
    let (arn, shard_id) = given_stream(&router, case, name).await;
    let session = given_session(&router).await;
    let document = denying(json!("kinesis:GetRecords"), &arn);

    // Before anything is attached, the identity document alone decides: the read is served.
    let iterator = fresh_iterator(&router, case, name, &shard_id).await;
    assert_outcome(
        &router,
        "before anything is attached",
        &session,
        &Call {
            label: "GetRecords",
            operation: "GetRecords",
            body: json!({ "ShardIterator": iterator }),
        },
        &arn,
        false,
    )
    .await;

    attach(&router, case, &arn, &document).await;
    // The document the resource now carries is the one that was attached, read back through
    // the service's own reader.
    let stored = owner_ok(
        &router,
        case,
        "GetResourcePolicy",
        &json!({ "ResourceARN": arn }),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(
            stored["Policy"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a stored Policy, got {stored}"))
        )
        .unwrap_or(Value::Null),
        serde_json::from_str::<Value>(&document).expect("the attached document is JSON"),
        "case {case:?}: the stream must carry the document that was attached, got {stored}"
    );

    let iterator = fresh_iterator(&router, case, name, &shard_id).await;
    assert_outcome(
        &router,
        "the deny attached",
        &session,
        &Call {
            label: "GetRecords",
            operation: "GetRecords",
            body: json!({ "ShardIterator": iterator }),
        },
        &arn,
        true,
    )
    .await;

    owner_ok(
        &router,
        case,
        "DeleteResourcePolicy",
        &json!({ "ResourceARN": arn }),
    )
    .await;

    // The identical call now reads the records the stream holds.
    let iterator = fresh_iterator(&router, case, name, &shard_id).await;
    let (status, answered) = kinesis_as(
        &router,
        &session.access_key_id,
        "GetRecords",
        &json!({ "ShardIterator": iterator }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the identical read must be served once the document is detached, got \
         {status} with {answered}"
    );
    let found = payloads_of(case, &answered);
    assert!(
        found.iter().any(|payload| payload == OWNER_RECORD),
        "case {case:?}: expected the session to read {OWNER_RECORD:?} once nothing denies it, \
         got {found:?}"
    );
}
