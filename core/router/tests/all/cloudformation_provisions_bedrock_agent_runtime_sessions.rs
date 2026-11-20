// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Bedrock::Session` deploys: the session is
//! minted in the Bedrock Agent RUNTIME plane, the stack publishes the identity that service
//! assigned, deleting the stack takes the session away — and a declaration naming an
//! encryption key AWS KMS holds no key for fails the resource with that service's own refusal
//! and rolls the stack back.
//!
//! The rules under test, one per table:
//!
//! 1. **A resource type AWS publishes is one a template may declare, and a declaration of it
//!    is deployed by calling the service that OWNS the type — so what the stack publishes is
//!    what that service answered, never a `Template format error: Unrecognized resource
//!    types`.** The rows of the deploy table differ along what that rule generalises over:
//!    which optional properties the declaration carries (none, the metadata map, the tags, an
//!    encryption key a client created in KMS), which published return value the output reads
//!    (`Ref`, `Fn::GetAtt SessionArn`, `Fn::GetAtt SessionId`), the template spelling (JSON
//!    with long-form intrinsics, YAML with the short-form tags) and how many sessions one
//!    stack declares.
//!
//! 2. **A session this plane cannot encrypt with the key the declaration names is REFUSED, and
//!    a resource whose create the owning service refused carries no physical resource id at
//!    all** — nothing minted one, so the stack reports none rather than the stand-in the
//!    naming rules computed.
//!
//! Three crates have to agree for either to hold (CloudFormation submits the template and
//! makes the calls, the Bedrock Agent runtime serves them, AWS KMS answers for the key), so
//! the suite lives here: a crate-local test can only ever see one side of it.
//!
//! Every wait here is a wait for a CONDITION — the stack's own status — under a deadline
//! generous enough that a loaded host cannot decide the outcome; nothing asserts how long a
//! step took.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::time::Instant;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stacks, the sessions and
/// the keys belong to.
const REGION: &str = "us-east-1";

/// How long the suite is willing to wait for a stack operation to settle, and how often it
/// looks again. Generous: a stack converges over as many clock-owed transitions as it has
/// dependency levels, and the suite waits for the CONDITION (the stack's status) rather than
/// for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(50);

/// The resource type under test, and the signing name every Bedrock-family model declares.
const SESSION_TYPE: &str = "AWS::Bedrock::Session";
const BEDROCK: &str = "bedrock";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The `SessionStatus` a freshly created session carries ("one of: ACTIVE | ENDED | EXPIRED",
/// `smithy_query(query="shape",shape="SessionStatus")`).
const ACTIVE: &str = "ACTIVE";

/// The sentence the Bedrock Agent runtime refuses a session whose encryption key it cannot use
/// with, and the SDK diagnostic the resource's failure reason carries it inside.
const KEY_ACCESS_DENIED: &str =
    "Check that your IAM role has the necessary KMS permissions in both the key policy and \
     IAM policy, then retry your request.";
const KEY_DENIED_DIAGNOSTIC: &str =
    "(Service: Bedrock Agent Runtime; Operation: CreateSession; Status Code: 403; Error Code: \
     AccessDeniedException; Request ID: ";

/// A key ARN inside `KmsKeyArn`'s own pattern whose key no account holds: the 36 characters
/// after `key/` are a uuid KMS never issued.
const ABSENT_KEY_ARN: &str =
    "arn:aws:kms:us-east-1:000000000000:key/11111111-2222-3333-4444-555555555555";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_bedrockagentruntime_amazonbedrockagentruntimeservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Bedrock Agent runtime operation on the URI its own `@http` binding names, over the
/// restJson1 wire that model declares.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body
        .map(|document| serde_json::to_vec(document).expect("the request body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header(
            "host",
            format!("bedrock-agent-runtime.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/json")
        .header("authorization", authorization(BEDROCK))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Call one KMS operation over its awsJson1_1 wire.
async fn kms(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header("authorization", authorization("kms"))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// A customer managed key this account and Region really hold: the ARN `kms:CreateKey`
/// answered for one it just issued.
async fn created_key_arn(router: &MultiServiceRouter, case: &str) -> String {
    let (status, created) = kms(router, "CreateKey", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from kms:CreateKey, got {status} with {created}"
    );
    created
        .get("KeyMetadata")
        .and_then(|metadata| metadata.get("Arn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: kms:CreateKey answered no key ARN: {created}"))
        .to_string()
}

/// `GetSession` on one identifier: the session that service reports, or `None` where it
/// reports no session at all.
///
/// The identifier is escaped as a single path segment, because `sessionIdentifier` is a
/// non-greedy `@httpLabel` and the ARN form of it carries `/`.
async fn get_session(router: &MultiServiceRouter, identifier: &str) -> Option<Value> {
    let (status, answered) = bedrock(
        router,
        "GET",
        &format!("/sessions/{}/", urlencoded(identifier)),
        None,
    )
    .await;
    (status == 200).then_some(answered)
}

/// Every session the Bedrock Agent runtime holds, as `ListSessions` reports them.
async fn list_sessions(router: &MultiServiceRouter) -> Vec<Value> {
    let (_, answered) = bedrock(router, "POST", "/sessions/", Some(&json!({}))).await;
    answered
        .get("sessionSummaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The tags that service holds on one session, as the map a client reads them as.
async fn tags_of(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let (_, answered) = bedrock(
        router,
        "GET",
        &format!("/tags/{}", urlencoded(arn)),
        None,
    )
    .await;
    answered
        .get("tags")
        .and_then(Value::as_object)
        .map(|tags| {
            tags.iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// One ARN as a single URI path segment, which is how an `@httpLabel` carrying one travels.
fn urlencoded(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            ':' => "%3A".to_string(),
            '/' => "%2F".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// The `DescribeStacks` answer once the stack rests in a status that is not in progress — or
/// once it is GONE, which a delete leaves it — or the last one read when the deadline passes.
/// Waits for the CONDITION, looking again until the stack has settled.
async fn settled_stack(router: &MultiServiceRouter, stack_name: &str) -> (u16, String) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        let settled = status != 200
            || opt_element(&body, "StackStatus")
                .is_some_and(|stack_status| !stack_status.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= deadline {
            return (status, body);
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Every `ResourceStatusReason` a stack has recorded, which is what a failed deployment has
/// to say for itself: the assertions below quote them, so a row that fails names the reason
/// the deployment gave rather than only the status it reached.
async fn failure_reasons(router: &MultiServiceRouter, stack_name: &str) -> Vec<String> {
    let (_, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    elements(&events, "ResourceStatusReason")
}

/// Create the stack and answer its settled `DescribeStacks` body, asserting the template was
/// ADMITTED: a type AWS publishes is never refused as an unrecognised one.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", template)],
    )
    .await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected {SESSION_TYPE} to be a type a template may declare, got {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let (status, settled) = settled_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {settled}"
    );
    settled
}

/// Whether `arn` has the form the Bedrock Agent runtime mints a session ARN in
/// (`SessionArn`: `^arn:aws(-[^:]+)?:bedrock:[a-z0-9-]+:[0-9]{12}:session/<uuid>$`), with the
/// uuid of the form `Uuid` fixes for a `sessionId`.
fn is_session_arn(arn: &str) -> bool {
    let Some(uuid) = arn.strip_prefix(&format!("arn:aws:bedrock:{REGION}:")) else {
        return false;
    };
    let Some((account, uuid)) = uuid.split_once(":session/") else {
        return false;
    };
    account.len() == 12
        && account.bytes().all(|byte| byte.is_ascii_digit())
        && is_session_uuid(uuid)
}

/// Whether `value` is a uuid of the form `Uuid` fixes
/// (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`).
fn is_session_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
}

/// What one row expects of one session the stack declares.
struct Declared {
    /// The output key whose value is the session's minted ARN.
    arn_output: &'static str,
    /// The output key whose value is the uuid that service minted, where the row reads one.
    id_output: Option<&'static str>,
    /// The metadata the declaration states, which the read must report.
    metadata: &'static [(&'static str, &'static str)],
    /// The tags it asks for, which that service's own tag read must answer with.
    tags: &'static [(&'static str, &'static str)],
    /// Whether the declaration names an encryption key, which is a key this suite created in
    /// KMS: the ARN is not known until then, so the row says only that it states one.
    encrypted: bool,
}

/// One row: a template and the sessions it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// Built from the ARN of the key this suite created, which the rows that state one
    /// interpolate.
    template: fn(&str) -> String,
    declared: Vec<Declared>,
}

fn rows() -> Vec<Row> {
    vec![
        // The bare declaration: a session with no property at all, whose identity is read back
        // through `Ref` — the type's primary identifier.
        Row {
            case: "a session with no properties, Ref read back",
            stack_name: "bedrock-session-bare",
            template: |_| {
                json!({
                    "AWSTemplateFormatVersion": "2010-09-09",
                    "Resources": { "Session": { "Type": SESSION_TYPE } },
                    "Outputs": { "SessionRef": { "Value": { "Ref": "Session" } } }
                })
                .to_string()
            },
            declared: vec![Declared {
                arn_output: "SessionRef",
                id_output: None,
                metadata: &[],
                tags: &[],
                encrypted: false,
            }],
        },
        // The metadata map and the type's own tags, with both published return values read
        // through `Fn::GetAtt`. The metadata keys carry the punctuation
        // `SessionMetadataKey` admits, so a key the caller chose reaches that service as
        // written rather than re-cased.
        Row {
            case: "declared metadata and tags, both attributes read back",
            stack_name: "bedrock-session-metadata",
            template: |_| {
                json!({
                    "Resources": {
                        "Session": {
                            "Type": SESSION_TYPE,
                            "Properties": {
                                "SessionMetadata": {
                                    "userId": "u-1234",
                                    "language.preference": "en-GB",
                                    "device_type": "tablet",
                                },
                                "Tags": { "team": "platform" },
                            }
                        }
                    },
                    "Outputs": {
                        "SessionArn": { "Value": { "Fn::GetAtt": ["Session", "SessionArn"] } },
                        "SessionId": { "Value": { "Fn::GetAtt": ["Session", "SessionId"] } }
                    }
                })
                .to_string()
            },
            declared: vec![Declared {
                arn_output: "SessionArn",
                id_output: Some("SessionId"),
                metadata: &[
                    ("device_type", "tablet"),
                    ("language.preference", "en-GB"),
                    ("userId", "u-1234"),
                ],
                tags: &[("team", "platform")],
                encrypted: false,
            }],
        },
        // The YAML spelling with the short-form intrinsic, carrying the encryption key: a key
        // this suite created through `kms:CreateKey`, which that service resolves.
        Row {
            case: "an encryption key a client created, YAML spelling",
            stack_name: "bedrock-session-encrypted",
            template: |key_arn| {
                format!(
                    "\
Resources:
  Session:
    Type: {SESSION_TYPE}
    Properties:
      EncryptionKeyArn: {key_arn}
      SessionMetadata:
        tenant: acme
Outputs:
  SessionArn:
    Value: !GetAtt Session.SessionArn
"
                )
            },
            declared: vec![Declared {
                arn_output: "SessionArn",
                id_output: None,
                metadata: &[("tenant", "acme")],
                tags: &[],
                encrypted: true,
            }],
        },
        // Two sessions in one stack, each publishing its OWN minted identity.
        Row {
            case: "two sessions in one stack",
            stack_name: "bedrock-session-pair",
            template: |_| {
                json!({
                    "Resources": {
                        "First": {
                            "Type": SESSION_TYPE,
                            "Properties": { "SessionMetadata": { "leg": "first" } }
                        },
                        "Second": {
                            "Type": SESSION_TYPE,
                            "Properties": { "SessionMetadata": { "leg": "second" } }
                        }
                    },
                    "Outputs": {
                        "FirstArn": { "Value": { "Fn::GetAtt": ["First", "SessionArn"] } },
                        "SecondArn": { "Value": { "Ref": "Second" } }
                    }
                })
                .to_string()
            },
            declared: vec![
                Declared {
                    arn_output: "FirstArn",
                    id_output: None,
                    metadata: &[("leg", "first")],
                    tags: &[],
                    encrypted: false,
                },
                Declared {
                    arn_output: "SecondArn",
                    id_output: None,
                    metadata: &[("leg", "second")],
                    tags: &[],
                    encrypted: false,
                },
            ],
        },
    ]
}

/// The text one member of a reported session carries.
fn reported(session: &Value, member: &str) -> Option<String> {
    session
        .get(member)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// The metadata one reported session carries, as the map a client reads it as.
fn reported_metadata(session: &Value) -> BTreeMap<String, String> {
    session
        .get("sessionMetadata")
        .and_then(Value::as_object)
        .map(|metadata| {
            metadata
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_session_the_bedrock_runtime_minted() {
    for row in rows() {
        let router = build_router();
        let case = row.case;
        let key_arn = created_key_arn(&router, case).await;
        let template = (row.template)(&key_arn);
        let body = create_and_settle(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body} with reasons {:?}",
            failure_reasons(&router, row.stack_name).await
        );
        let outputs: BTreeMap<String, String> = outputs_of(&body);

        let (_, resources) = cloudformation(
            &router,
            "DescribeStackResources",
            &[("StackName", row.stack_name)],
        )
        .await;
        let physical_ids = physical_ids_of(&resources);

        for declared in &row.declared {
            let arn = outputs
                .get(declared.arn_output)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no {} output in {body}", declared.arn_output)
                })
                .clone();
            assert!(
                is_session_arn(&arn),
                "case {case:?}: expected the published value to be an ARN of the modelled \
                 session form, got {arn:?}"
            );
            assert!(
                physical_ids.values().any(|physical| *physical == arn),
                "case {case:?}: expected the ARN that service minted to be the resource's \
                 physical id, got {physical_ids:?}"
            );

            // The identity the stack published is one the OWNING service resolves, and the
            // session it resolves to is the one the template declared.
            let session = get_session(&router, &arn).await.unwrap_or_else(|| {
                panic!("case {case:?}: expected the runtime plane to hold the published {arn:?}")
            });
            assert_eq!(
                reported(&session, "sessionArn").as_deref(),
                Some(arn.as_str()),
                "case {case:?}: expected the read to report the published ARN, got {session}"
            );
            assert_eq!(
                reported(&session, "sessionStatus").as_deref(),
                Some(ACTIVE),
                "case {case:?}: expected a created session to be {ACTIVE}, got {session}"
            );
            let session_id = reported(&session, "sessionId").unwrap_or_default();
            assert!(
                is_session_uuid(&session_id) && arn.ends_with(&session_id),
                "case {case:?}: expected the ARN to carry the uuid that service minted, got \
                 {arn:?} and {session_id:?}"
            );
            if let Some(id_output) = declared.id_output {
                assert_eq!(
                    outputs.get(id_output).map(String::as_str),
                    Some(session_id.as_str()),
                    "case {case:?}: expected the published {id_output} to be the minted uuid, \
                     got {outputs:?}"
                );
            }
            assert_eq!(
                reported_metadata(&session),
                declared
                    .metadata
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                    .collect::<BTreeMap<String, String>>(),
                "case {case:?}: expected the declared metadata, got {session}"
            );
            assert_eq!(
                reported(&session, "encryptionKeyArn"),
                declared.encrypted.then(|| key_arn.clone()),
                "case {case:?}: expected the declared encryption key, got {session}"
            );

            // The tags the declaration asked for reached that service, beside the three
            // CloudFormation records on every resource it provisions.
            let held = tags_of(&router, &arn).await;
            for (key, value) in declared.tags {
                assert_eq!(
                    held.get(*key).map(String::as_str),
                    Some(*value),
                    "case {case:?}: expected the declared tag {key:?}, got {held:?}"
                );
            }
            assert_eq!(
                held.get("aws:cloudformation:stack-name").map(String::as_str),
                Some(row.stack_name),
                "case {case:?}: expected the stack's own tag on the session, got {held:?}"
            );
        }

        // Deleting the stack takes every session it minted away.
        let (status, deleted) =
            cloudformation(&router, "DeleteStack", &[("StackName", row.stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {deleted}"
        );
        let (_, settled) = settled_stack(&router, row.stack_name).await;
        assert!(
            opt_element(&settled, "StackStatus")
                .is_none_or(|stack_status| stack_status == "DELETE_COMPLETE"),
            "case {case:?}: expected the stack to be deleted, got {settled}"
        );
        for declared in &row.declared {
            let arn = outputs
                .get(declared.arn_output)
                .expect("the output was read above");
            assert!(
                get_session(&router, arn).await.is_none(),
                "case {case:?}: expected the deleted stack's session {arn:?} to be gone"
            );
        }
        assert!(
            list_sessions(&router).await.is_empty(),
            "case {case:?}: expected the runtime plane to hold no session after the delete"
        );
    }
}

#[tokio::test]
async fn a_changed_declaration_updates_the_session_in_place() {
    let case = "the metadata a re-declaration states";
    let router = build_router();
    let stack = "bedrock-session-updated";
    let template = |metadata: Value| {
        json!({
            "Resources": {
                "Session": {
                    "Type": SESSION_TYPE,
                    "Properties": { "SessionMetadata": metadata }
                }
            },
            "Outputs": { "SessionArn": { "Value": { "Ref": "Session" } } }
        })
        .to_string()
    };

    let body = create_and_settle(
        &router,
        case,
        stack,
        &template(json!({ "turn": "first" })),
    )
    .await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete, got {body} with reasons {:?}",
        failure_reasons(&router, stack).await
    );
    let arn = outputs_of(&body)
        .get("SessionArn")
        .unwrap_or_else(|| panic!("case {case:?}: no SessionArn output in {body}"))
        .clone();

    let (status, updated) = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template(json!({ "turn": "second" }))),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {updated}"
    );
    let (_, settled) = settled_stack(&router, stack).await;
    assert_eq!(
        opt_element(&settled, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the update to complete, got {settled}"
    );

    // The session keeps the identity that service minted, and carries the metadata the
    // re-declaration states rather than the one the first deployment gave it.
    assert_eq!(
        outputs_of(&settled).get("SessionArn"),
        Some(&arn),
        "case {case:?}: expected the session to keep the identity that service minted, got \
         {settled}"
    );
    let session = get_session(&router, &arn)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the runtime plane to still hold {arn:?}"));
    assert_eq!(
        reported_metadata(&session).get("turn").map(String::as_str),
        Some("second"),
        "case {case:?}: expected the re-declared metadata, got {session}"
    );
    assert_eq!(
        list_sessions(&router).await.len(),
        1,
        "case {case:?}: expected the one session to be changed rather than a second one minted"
    );
}

#[tokio::test]
async fn an_encryption_key_kms_holds_no_key_for_rolls_the_stack_back() {
    let case = "an encryption key no account holds";
    let router = build_router();
    let stack = "bedrock-session-absent-key";
    let template = json!({
        "Resources": {
            "Session": {
                "Type": SESSION_TYPE,
                "Properties": { "EncryptionKeyArn": ABSENT_KEY_ARN }
            }
        },
        "Outputs": { "SessionRef": { "Value": { "Ref": "Session" } } }
    })
    .to_string();

    let body = create_and_settle(&router, case, stack, &template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the failed create to be rolled back rather than publish an \
         identity nothing minted, got {body}"
    );
    assert!(
        outputs_of(&body).is_empty(),
        "case {case:?}: expected no output for a resource nothing created, got {body}"
    );

    // The failure is the OWNING service's own refusal, on the resource that failed, inside the
    // diagnostic its SDK words — never a refusal of the template.
    let (_, events) = cloudformation(&router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert!(
        !events.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected no template-format error for a type AWS publishes, got {events}"
    );
    let reasons = elements(&events, "ResourceStatusReason");
    assert!(
        reasons.iter().any(|reason| reason.contains(KEY_ACCESS_DENIED)
            && reason.contains(KEY_DENIED_DIAGNOSTIC)
            && reason.contains("HandlerErrorCode: AccessDenied")),
        "case {case:?}: expected a failure reason carrying {KEY_ACCESS_DENIED:?} inside the \
         SDK diagnostic, got {reasons:?}"
    );

    // Nothing minted an identity for it, so the resource reports no physical id at all...
    let (_, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        physical_ids_of(&resources).get("Session").map(String::as_str),
        None,
        "case {case:?}: expected the refused session to carry no PhysicalResourceId, got \
         {resources}"
    );
    // ...and nothing was created: that service holds no session at all.
    assert!(
        list_sessions(&router).await.is_empty(),
        "case {case:?}: expected the runtime plane to hold no session, got some"
    );
}
