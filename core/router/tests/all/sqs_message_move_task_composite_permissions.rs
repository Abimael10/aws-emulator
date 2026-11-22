// SPDX-License-Identifier: CC0-1.0

//! What a caller has to be permitted before Amazon SQS starts a message movement task for
//! it, asserted end to end over Amazon SQS beside the IAM and STS services the principals
//! are made with, on one engine.
//!
//! The rule under test: a message movement task is a COMPOSITE of what it performs, so
//! `StartMessageMoveTask` is served only to a caller the policies in force permit every one
//! of those actions on the queue it performs it on — starting the task on the source queue,
//! taking each message out of it (`sqs:ReceiveMessage`, `sqs:DeleteMessage`), and delivering
//! it into the destination queue (`sqs:SendMessage`). A caller holding only part of that
//! grant is refused HTTP 403 `AccessDenied` with IAM's own sentence, naming the action and
//! the ARN of the QUEUE whose grant was missing, and no task is recorded at all.
//!
//! Only a cross-service suite can pin it: the principals are made with IAM's and STS's own
//! operations over the same engine (`CreateUser` + `PutUserPolicy` + `CreateAccessKey`,
//! `CreateRole` + `PutRolePolicy` + `AssumeRole`), so the documents Amazon SQS decides on
//! are the ones those services wrote and the principal a refusal blames is the identity STS
//! reports for the credential that signed.
//!
//! The rows differ along what the rule generalises over: WHICH of the four (action, queue)
//! pairs the grant withholds, where the grant comes from (the caller's identity-based policy
//! or the destination queue's own `Policy`), the resource the grant is scoped to (each
//! queue's own ARN, every queue of the account, a third queue, every resource), the effect
//! and its order (an `Allow` of everything beside an explicit `Deny` of one action), whether
//! the request NAMES the destination or leaves it blank — redriving back to the queue that
//! drove its messages into the dead-letter queue — and the kind of credential presented (a
//! user's long-term key, an assumed-role session). Each row runs on a FRESH emulator, so one
//! row's documents never decide another row's call.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sqs_amazonsqs::scaffolded as refusals;

use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for, so the ARNs the decisions are made about are
/// the ones this Region's queues carry.
const REGION: &str = "us-east-1";

/// An access-key id neither IAM nor STS holds a record for: it resolves to the account's
/// ambient identity, which holds no IAM policies and full control of its own queues — the
/// position AWS account root credentials are in. Every setup and read-back leg presents it.
const ACCOUNT_KEY: &str = "AKIDEXAMPLEACCOUNT";

/// The session name every assumed-role credential below carries.
const SESSION_NAME: &str = "probe";

/// How many deliveries drive a message of the source queue into its dead-letter queue, which
/// is what makes the dead-letter queue a source a move may be started on.
const MAX_RECEIVE_COUNT: u32 = 1;

/// The actions a move performs, each on the queue it performs it on. The move's own action
/// and the two that take a message out of the source queue are charged on the SOURCE queue;
/// the send that delivers a moved message is charged on the DESTINATION queue.
const START: &str = "sqs:StartMessageMoveTask";
const RECEIVE: &str = "sqs:ReceiveMessage";
const DELETE: &str = "sqs:DeleteMessage";
const SEND: &str = "sqs:SendMessage";

/// The emulator these rows drive: Amazon SQS beside the IAM and STS services the principals
/// and their policies are made with, on ONE engine, as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here signs: the `Authorization` header carries only the credential scope,
    // which is where a service reads the caller's access key id from.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(Arc::clone(&engine)),
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

/// A SigV4-shaped `Authorization` header: its credential scope names the service, which is
/// what routes the request, and carries the access key id a service resolves the caller from.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0"
    )
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) presenting `access_key_id`, as
/// its status and parsed body.
async fn sqs(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization(access_key_id, "sqs"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
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

/// One Amazon SQS call signed as the account itself, which must succeed: the suite's setup
/// and read-back legs.
async fn served(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = sqs(router, ACCOUNT_KEY, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the account's own {operation} must succeed, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call to IAM or STS, signed with the account's own credential, which must
/// succeed: these are the suite's setup legs.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(ACCOUNT_KEY, signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {action:?}: the setup call must succeed, got {status} with {text}"
    );
    text
}

/// A principal a call is signed as: the access key id it presents, and the ARN a refusal
/// must blame it by — what `sts:GetCallerIdentity` reports for that key.
struct Principal {
    access_key_id: String,
    arn: String,
}

/// An IAM user carrying `policy` as its inline policy, with a long-term access key.
async fn user(router: &MultiServiceRouter, user_name: &str, policy: &str) -> Principal {
    let created = query(
        router,
        "iam",
        "2010-05-08",
        "CreateUser",
        &[("UserName", user_name.to_string())],
    )
    .await;
    let arn = element(&created, "Arn");
    query(
        router,
        "iam",
        "2010-05-08",
        "PutUserPolicy",
        &[
            ("UserName", user_name.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    let minted = query(
        router,
        "iam",
        "2010-05-08",
        "CreateAccessKey",
        &[("UserName", user_name.to_string())],
    )
    .await;
    Principal {
        access_key_id: element(&minted, "AccessKeyId"),
        arn,
    }
}

/// An IAM role carrying `policy` as its inline policy, assumed through STS: the session's
/// own credential and the ARN STS reports for it.
async fn assumed_role(router: &MultiServiceRouter, role_name: &str, policy: &str) -> Principal {
    let account = engine::emulator_account_id();
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{account}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    let assumed = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            (
                "RoleArn",
                format!("arn:aws:iam::{account}:role/{role_name}"),
            ),
            ("RoleSessionName", SESSION_NAME.to_string()),
        ],
    )
    .await;
    Principal {
        access_key_id: element(&assumed, "AccessKeyId"),
        arn: element(&assumed, "Arn"),
    }
}

/// The queues one row's decision is taken about: the dead-letter queue a move reads from,
/// the queue that drives its messages there (which is also the move's destination), and a
/// third queue no row addresses — what a grant scoped elsewhere is written about.
struct Fixture {
    source_arn: String,
    destination_arn: String,
    destination_url: String,
    other_arn: String,
}

/// Create the queue `name` and answer `(its url, its ARN)`.
async fn queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = served(router, case, "CreateQueue", &json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue answered {created}"))
        .to_string();
    let reported = served(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = reported["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: GetQueueAttributes answered {reported}"))
        .to_string();
    (url, arn)
}

/// The three queues of one row, with the dead-letter relationship in place and one message
/// waiting on the dead-letter queue, so a started task has something to move.
async fn fixture(router: &MultiServiceRouter, case: &str, index: usize) -> Fixture {
    let (source_url, source_arn) = queue(router, case, &format!("smmt-dlq-{index}")).await;
    let (destination_url, destination_arn) =
        queue(router, case, &format!("smmt-main-{index}")).await;
    let (_, other_arn) = queue(router, case, &format!("smmt-other-{index}")).await;
    served(
        router,
        case,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": destination_url,
            "Attributes": {
                "RedrivePolicy": json!({
                    "deadLetterTargetArn": source_arn,
                    "maxReceiveCount": MAX_RECEIVE_COUNT,
                })
                .to_string(),
            },
        }),
    )
    .await;
    served(
        router,
        case,
        "SendMessage",
        &json!({"QueueUrl": source_url, "MessageBody": "dead-lettered"}),
    )
    .await;
    Fixture {
        source_arn,
        destination_arn,
        destination_url,
        other_arn,
    }
}

/// A policy document carrying `statements`.
fn document(statements: Value) -> String {
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// A statement about `actions` on `resource`.
fn statement(effect: &str, actions: Value, resource: &str) -> Value {
    json!({"Effect": effect, "Action": actions, "Resource": resource})
}

/// The whole grant a move needs: the move itself and the two actions that take a message out
/// of the source queue, on the source queue; the delivery, on the destination queue.
fn complete_grant(fixture: &Fixture) -> String {
    document(json!([
        statement("Allow", json!([START, RECEIVE, DELETE]), &fixture.source_arn),
        statement("Allow", json!(SEND), &fixture.destination_arn),
    ]))
}

/// Which of the two queues an answer names.
#[derive(Debug, Clone, Copy)]
enum Side {
    Source,
    Destination,
}

impl Side {
    /// The ARN of the queue this side stands for in one row's fixture.
    fn arn(self, fixture: &Fixture) -> String {
        match self {
            Self::Source => fixture.source_arn.clone(),
            Self::Destination => fixture.destination_arn.clone(),
        }
    }
}

/// The refusal a row expects: the action the policies in force withheld, the queue it was
/// withheld on, and the clause naming the document that withheld it.
struct Refusal {
    action: &'static str,
    side: Side,
    reason: &'static str,
}

/// The clause an IMPLICIT deny ends with — no document in force allows the action.
const NO_IDENTITY_POLICY: &str = "because no identity-based policy allows the";

/// The clause an EXPLICIT deny in one of the caller's own documents ends with.
const EXPLICIT_IDENTITY_DENY: &str = "with an explicit deny in an identity-based policy";

impl Refusal {
    /// The whole sentence this refusal has to carry, as IAM words it: the principal, the
    /// action in the lower case this service names it in, the queue's ARN, and the fact
    /// about the policies in force that refused the call.
    fn message(&self, principal: &str, fixture: &Fixture) -> String {
        let action = self.action.to_lowercase();
        let resource = self.side.arn(fixture);
        let reason = if self.reason == NO_IDENTITY_POLICY {
            format!("{NO_IDENTITY_POLICY} {action} action")
        } else {
            self.reason.to_string()
        };
        format!(
            "User: {principal} is not authorized to perform: {action} on resource: {resource} \
             {reason}"
        )
    }
}

/// One row of the table: the documents in force, the form of the request, and what the call
/// must answer.
struct Row {
    case: &'static str,
    /// The identity-based policy the calling principal carries.
    identity: fn(&Fixture) -> String,
    /// The `Policy` the DESTINATION queue carries, built from the caller's own ARN. `None`
    /// leaves the queue with no document of its own.
    destination_policy: Option<fn(&Fixture, &str) -> String>,
    /// Whether the request names `DestinationArn`. A blank one redrives the messages back to
    /// the queue that drove them into the dead-letter queue, which is the same queue here.
    names_destination: bool,
    /// Whether the call presents an assumed-role session rather than a user's long-term key.
    session: bool,
    /// `None` when the task is started; otherwise the refusal the call must answer.
    refusal: Option<Refusal>,
}

/// Every message movement task the dead-letter queue `source_arn` holds, read back as the
/// account.
async fn tasks_on(router: &MultiServiceRouter, case: &str, source_arn: &str) -> Vec<Value> {
    let listed = served(
        router,
        case,
        "ListMessageMoveTasks",
        &json!({"SourceArn": source_arn}),
    )
    .await;
    listed["Results"].as_array().cloned().unwrap_or_default()
}

/// RULE: `StartMessageMoveTask` is served only to a caller the policies in force permit
/// every action the move performs on the queue it performs it on — the move itself, the
/// receive and the delete on the source queue, and the send on the destination queue — and a
/// caller holding part of that grant is refused `AccessDenied` at HTTP 403, naming the
/// action and the queue whose grant was missing, with no task recorded.
#[tokio::test]
async fn a_move_is_started_only_for_a_caller_permitted_on_both_queues() {
    let rows: &[Row] = &[
        Row {
            case: "every action the move performs, each on the queue it performs it on",
            identity: complete_grant,
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: None,
        },
        Row {
            case: "the same complete grant, with the destination left blank",
            identity: complete_grant,
            destination_policy: None,
            names_destination: false,
            session: false,
            refusal: None,
        },
        Row {
            case: "every action of this service on each of the two queues",
            identity: |fixture| {
                document(json!([
                    statement("Allow", json!("sqs:*"), &fixture.source_arn),
                    statement("Allow", json!("sqs:*"), &fixture.destination_arn),
                ]))
            },
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: None,
        },
        Row {
            case: "every action of this service on every resource",
            identity: |_| document(json!([statement("Allow", json!("sqs:*"), "*")])),
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: None,
        },
        Row {
            case: "the move's own action alone on the source queue",
            identity: |fixture| {
                document(json!([statement(
                    "Allow",
                    json!(START),
                    &fixture.source_arn
                )]))
            },
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: Some(Refusal {
                action: RECEIVE,
                side: Side::Source,
                reason: NO_IDENTITY_POLICY,
            }),
        },
        Row {
            case: "the move and the receive on the source queue, without the delete",
            identity: |fixture| {
                document(json!([statement(
                    "Allow",
                    json!([START, RECEIVE]),
                    &fixture.source_arn
                )]))
            },
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: Some(Refusal {
                action: DELETE,
                side: Side::Source,
                reason: NO_IDENTITY_POLICY,
            }),
        },
        Row {
            case: "every action of this service on the source queue and nothing on the \
                   destination",
            identity: |fixture| {
                document(json!([statement(
                    "Allow",
                    json!("sqs:*"),
                    &fixture.source_arn
                )]))
            },
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: Some(Refusal {
                action: SEND,
                side: Side::Destination,
                reason: NO_IDENTITY_POLICY,
            }),
        },
        Row {
            case: "the source queue's whole grant with the destination left blank, which \
                   redrives into a queue nothing grants",
            identity: |fixture| {
                document(json!([statement(
                    "Allow",
                    json!("sqs:*"),
                    &fixture.source_arn
                )]))
            },
            destination_policy: None,
            names_destination: false,
            session: false,
            refusal: Some(Refusal {
                action: SEND,
                side: Side::Destination,
                reason: NO_IDENTITY_POLICY,
            }),
        },
        Row {
            case: "every action of this service on the destination queue alone",
            identity: |fixture| {
                document(json!([statement(
                    "Allow",
                    json!("sqs:*"),
                    &fixture.destination_arn
                )]))
            },
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: Some(Refusal {
                action: START,
                side: Side::Source,
                reason: NO_IDENTITY_POLICY,
            }),
        },
        Row {
            case: "a complete grant written about a third queue",
            identity: |fixture| {
                document(json!([statement(
                    "Allow",
                    json!([START, RECEIVE, DELETE, SEND]),
                    &fixture.other_arn
                )]))
            },
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: Some(Refusal {
                action: START,
                side: Side::Source,
                reason: NO_IDENTITY_POLICY,
            }),
        },
        Row {
            case: "a document that says nothing about this service",
            identity: |_| document(json!([statement("Allow", json!("s3:*"), "*")])),
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: Some(Refusal {
                action: START,
                side: Side::Source,
                reason: NO_IDENTITY_POLICY,
            }),
        },
        Row {
            case: "the complete grant with an explicit deny of the delete on the source \
                   queue written after it",
            identity: |fixture| {
                document(json!([
                    statement("Allow", json!([START, RECEIVE, DELETE]), &fixture.source_arn),
                    statement("Allow", json!(SEND), &fixture.destination_arn),
                    statement("Deny", json!(DELETE), &fixture.source_arn),
                ]))
            },
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: Some(Refusal {
                action: DELETE,
                side: Side::Source,
                reason: EXPLICIT_IDENTITY_DENY,
            }),
        },
        Row {
            case: "the complete grant with an explicit deny of the send on the destination \
                   queue written before it",
            identity: |fixture| {
                document(json!([
                    statement("Deny", json!(SEND), &fixture.destination_arn),
                    statement("Allow", json!("sqs:*"), "*"),
                ]))
            },
            destination_policy: None,
            names_destination: true,
            session: false,
            refusal: Some(Refusal {
                action: SEND,
                side: Side::Destination,
                reason: EXPLICIT_IDENTITY_DENY,
            }),
        },
        Row {
            case: "the source queue's grant in the identity policy and the destination's in \
                   that queue's own policy",
            identity: |fixture| {
                document(json!([statement(
                    "Allow",
                    json!([START, RECEIVE, DELETE]),
                    &fixture.source_arn
                )]))
            },
            destination_policy: Some(|fixture, caller| {
                document(json!([json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": caller},
                    "Action": SEND,
                    "Resource": &fixture.destination_arn,
                })]))
            }),
            names_destination: true,
            session: false,
            refusal: None,
        },
        Row {
            case: "the destination queue's own policy granting the caller another action of \
                   this service",
            identity: |fixture| {
                document(json!([statement(
                    "Allow",
                    json!([START, RECEIVE, DELETE]),
                    &fixture.source_arn
                )]))
            },
            destination_policy: Some(|fixture, caller| {
                document(json!([json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": caller},
                    "Action": RECEIVE,
                    "Resource": &fixture.destination_arn,
                })]))
            }),
            names_destination: true,
            session: false,
            refusal: Some(Refusal {
                action: SEND,
                side: Side::Destination,
                reason: NO_IDENTITY_POLICY,
            }),
        },
        Row {
            case: "an assumed-role session holding the complete grant",
            identity: complete_grant,
            destination_policy: None,
            names_destination: true,
            session: true,
            refusal: None,
        },
        Row {
            case: "an assumed-role session granted on the source queue alone",
            identity: |fixture| {
                document(json!([statement(
                    "Allow",
                    json!("sqs:*"),
                    &fixture.source_arn
                )]))
            },
            destination_policy: None,
            names_destination: true,
            session: true,
            refusal: Some(Refusal {
                action: SEND,
                side: Side::Destination,
                reason: NO_IDENTITY_POLICY,
            }),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = build_router();
        let fixture = fixture(&router, case, index).await;
        let policy = (row.identity)(&fixture);
        let principal = if row.session {
            assumed_role(&router, &format!("smmt-role-{index}"), &policy).await
        } else {
            user(&router, &format!("smmt-user-{index}"), &policy).await
        };
        if let Some(destination_policy) = row.destination_policy {
            served(
                &router,
                case,
                "SetQueueAttributes",
                &json!({
                    "QueueUrl": fixture.destination_url,
                    "Attributes": {
                        "Policy": destination_policy(&fixture, &principal.arn),
                    },
                }),
            )
            .await;
        }

        let mut request = json!({"SourceArn": fixture.source_arn});
        if row.names_destination {
            request["DestinationArn"] = json!(fixture.destination_arn);
        }
        let (status, body) = sqs(
            &router,
            &principal.access_key_id,
            "StartMessageMoveTask",
            &request,
        )
        .await;

        let Some(refusal) = &row.refusal else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the move to be started, got {status} with {body}"
            );
            let handle = body["TaskHandle"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a TaskHandle, got {body}"));
            // Read the task back through the service's own reader: a start that answered a
            // handle is a task the store holds, on the queue the request named.
            let tasks = tasks_on(&router, case, &fixture.source_arn).await;
            assert_eq!(
                tasks.len(),
                1,
                "case {case:?}: expected exactly the started task, got {tasks:?}"
            );
            assert_eq!(
                tasks[0]["TaskHandle"].as_str(),
                Some(handle),
                "case {case:?}: expected the task the start answered, got {:?}",
                tasks[0]
            );
            assert_eq!(
                tasks[0]["SourceArn"].as_str(),
                Some(fixture.source_arn.as_str()),
                "case {case:?}: expected the task to name the source queue, got {:?}",
                tasks[0]
            );
            continue;
        };

        assert_eq!(
            (status, body["__type"].as_str()),
            (
                refusals::ACCESS_DENIED_STATUS,
                Some(refusals::ACCESS_DENIED_SHAPE_ID)
            ),
            "case {case:?}: expected a {} {}, got {status} with {body}",
            refusals::ACCESS_DENIED_STATUS,
            refusals::ACCESS_DENIED
        );
        assert_eq!(
            body["Message"].as_str(),
            Some(refusal.message(&principal.arn, &fixture).as_str()),
            "case {case:?}: expected IAM's sentence about {:?} on the {:?} queue, got {body}",
            refusal.action,
            refusal.side
        );
        // A refused call records nothing: the store holds no task for that source queue.
        let tasks = tasks_on(&router, case, &fixture.source_arn).await;
        assert!(
            tasks.is_empty(),
            "case {case:?}: a refused start records no task, got {tasks:?}"
        );
    }
}
