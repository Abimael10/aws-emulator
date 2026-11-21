// SPDX-License-Identifier: CC0-1.0

//! What ONE deployed `AWS_IAM`-authorized method of an API Gateway REST API stage owes every
//! identity that reaches it, whether the request is a client's own or one EventBridge makes for
//! a rule's target.
//!
//! RULE: a request to a deployed method whose `authorizationType` is `AWS_IAM` is ANSWERED, and
//! is decided by the identity its credential speaks for alone: an identity allowed
//! `execute-api:Invoke` on that method's own ARN reaches the integration behind it, and one that
//! is not is answered `403` and reaches nothing. How the request arrived changes neither — a
//! client's own signed request is answered to the client, at whichever address of the stage it
//! dialled, while a request EventBridge makes AS a target's role ("for EC2 instances, Kinesis
//! data streams, Step Functions state machines and API Gateway APIs, EventBridge relies on IAM
//! roles that you specify in the `RoleARN` argument in `PutTargets`") is answered to EventBridge,
//! which records the refusal on the target's dead-letter queue with `ERROR_CODE`
//! `NO_PERMISSIONS` and the sentence it words that status in.
//!
//! Only a cross-service suite can pin this: the stage and its invoke endpoint are API Gateway's,
//! the user and the roles whose documents decide each request are IAM's, the rule, its target and
//! the dead letter are EventBridge's, and the sink the method writes into — which is what shows
//! WHAT reached the integration rather than what the gate said about it — is Amazon SQS's. The
//! stage, its queues and the wording of a refused invoke are the ones
//! [`crate::events_execute_api_target_role_grant`] already stands up and spells, imported rather
//! than written again; what is this suite's own is the CLIENT'S OWN request at that same stage —
//! the leg a caller observes directly, and the one whose absence makes a stage that never answers
//! indistinguishable from one that refused.
//!
//! The rows vary what the rule generalises over: WHICH identity the request is made under (a user
//! of this account allowed the invoke on the method's own ARN, one allowed only another action
//! there, one holding no document at all, a target role allowed the invoke, a target role allowed
//! only another action), HOW the request arrives (dialled by the client at the api's own invoke
//! host or at the address this emulator serves the stage at, or made by EventBridge for a matched
//! event), and WHICH resource of the method's path it addresses.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::events_execute_api_target_role_grant::{
    assert_forbidden_sentence, deployed, drained, published, Deployed, INVOKE_ACTION,
    MAX_EVENT_AGE_SECONDS, RETRIES_ALLOWED,
};
use crate::events_rest_api_stage_target::{
    attribute, emulator, events, iam, messages, policy, role, DETAIL_TYPE, EVENTS_TRUST, JSON,
    NO_PERMISSIONS, REGION, SOURCE_HEADER, STAGE,
};

/// The API Gateway version every awsQuery call below carries.
const IAM_VERSION: &str = "2010-05-08";

/// The clause IAM words an authorization failure in, which is what a refused request carries.
const NOT_AUTHORIZED_CLAUSE: &str = "is not authorized to perform: execute-api:Invoke";

/// Another action of the same service, which is a document granting a role everything except the
/// one action an invoke performs.
const OTHER_ACTION: &str = "execute-api:InvalidateCache";

/// How long a dial of the stage keeps being retried before the suite gives up on it: generous,
/// because what it waits for is a CONDITION — a deployment that exists is reachable — and not a
/// duration.
const SERVED_DEADLINE: Duration = Duration::from_secs(30);

/// How long such a dial waits before trying again.
const SERVED_INTERVAL: Duration = Duration::from_millis(20);

/// Where a client dials one method of the deployed stage.
#[derive(Clone, Copy, Debug)]
enum Address {
    /// The api's own invoke host, which is how a client outside this process addresses it.
    InvokeHost,
    /// The address this emulator serves every stage at, which is what its endpoint publishes.
    DataPlanePath,
}

impl Address {
    /// The `Host` the request carries.
    fn host(self, api: &str) -> String {
        match self {
            Self::InvokeHost => format!("{api}.execute-api.{REGION}.amazonaws.com"),
            Self::DataPlanePath => format!("apigateway.{REGION}.amazonaws.com"),
        }
    }

    /// The path the request addresses `/orders/{id}` of the stage by.
    fn path(self, api: &str, id: &str) -> String {
        match self {
            Self::InvokeHost => format!("/{STAGE}/orders/{id}"),
            Self::DataPlanePath => format!("/_aws/execute-api/{api}/{STAGE}/orders/{id}"),
        }
    }
}

/// How the request at the method is made.
#[derive(Clone, Copy, Debug)]
enum Arrival {
    /// A client's own signed request, dialled at this address.
    Signed(Address),
    /// The request EventBridge makes for a matched event, as the target's role.
    Delivered,
}

/// One row: how the request arrives, which identity it is made under, what it contributes as
/// [`SOURCE_HEADER`], which resource of the method's path it addresses, and whether the method
/// serves it.
struct Row {
    case: &'static str,
    arrival: Arrival,
    /// The document in force on the identity the request is made under, built from the method's
    /// own ARN — `None` for an identity carrying no identity-based document at all.
    document: Option<fn(&Deployed) -> String>,
    header: &'static str,
    id: &'static str,
    served: bool,
}

/// The credential scope a signed request carries for `access_key_id`. The signature itself is
/// never checked — this router runs with request authentication off, as every suite here runs it
/// — but the KEY ID is what the invoke resolves its principal from.
fn credential_scope(access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/execute-api/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One IAM user of this account holding a long-term access key and, when it carries one,
/// `document` as its one inline policy — answering the key id a request of that user signs with.
/// `None` is a user carrying no identity-based document at all.
async fn signing_user(router: &MultiServiceRouter, name: &str, document: Option<&str>) -> String {
    iam(
        router,
        &format!("CreateUser({name})"),
        format!("Action=CreateUser&Version={IAM_VERSION}&UserName={name}"),
    )
    .await;
    if let Some(document) = document {
        iam(
            router,
            &format!("PutUserPolicy({name})"),
            format!(
                "Action=PutUserPolicy&Version={IAM_VERSION}&UserName={name}\
                 &PolicyName=inline&PolicyDocument={document}"
            ),
        )
        .await;
    }
    let created = iam(
        router,
        &format!("CreateAccessKey({name})"),
        format!("Action=CreateAccessKey&Version={IAM_VERSION}&UserName={name}"),
    )
    .await;
    let open = "<AccessKeyId>";
    let close = "</AccessKeyId>";
    let start = created
        .find(open)
        .unwrap_or_else(|| panic!("setup CreateAccessKey({name}): no {open} in {created}"))
        + open.len();
    let end = created[start..]
        .find(close)
        .unwrap_or_else(|| panic!("setup CreateAccessKey({name}): no {close} in {created}"))
        + start;
    created[start..end].to_string()
}

/// Dial `POST /orders/{id}` of the deployed stage once, as `access_key_id`, and answer the status
/// and the body that came back.
async fn signed_post(
    router: &MultiServiceRouter,
    api: &str,
    address: Address,
    access_key_id: &str,
    id: &str,
    header: &str,
    body: &Value,
) -> (u16, String) {
    let host = address.host(api);
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}{}", address.path(api, id)))
        .header("host", host)
        .header("authorization", credential_scope(access_key_id))
        .header("content-type", JSON)
        .header(SOURCE_HEADER, header)
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(&response.into_body()).to_string(),
    )
}

/// Dial the stage until it answers `200`, or [`SERVED_DEADLINE`] passes — the one condition this
/// suite waits on at the stage, so a row never depends on how fast the host runs.
async fn served_within(
    router: &MultiServiceRouter,
    api: &str,
    address: Address,
    access_key_id: &str,
    id: &str,
    header: &str,
    body: &Value,
) -> (u16, String) {
    let deadline = Instant::now() + SERVED_DEADLINE;
    loop {
        let answered = signed_post(router, api, address, access_key_id, id, header, body).await;
        if answered.0 == 200 || Instant::now() >= deadline {
            return answered;
        }
        ::tokio::time::sleep(SERVED_INTERVAL).await;
    }
}

/// The sentence an API Gateway refusal carries, or the whole body when it carries none.
fn message_of(text: &str) -> String {
    let body: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    for member in ["Message", "message"] {
        if let Some(message) = body.get(member).and_then(Value::as_str) {
            return message.to_string();
        }
    }
    text.to_string()
}

/// The document a message carries as its body, which for a message at the sink is the one the
/// method's integration template wrote out of the request the method served.
fn body_of(case: &str, message: &Value) -> Value {
    message["Body"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a document as the body, got {message}"))
}

/// Wire one rule of the custom bus to the stage's method as `role_arn`, contributing `header` and
/// dead-lettering to the stage's own queue, and answer the rule's ARN.
async fn wired(
    router: &MultiServiceRouter,
    case: &str,
    stage: &Deployed,
    name: &str,
    source: &str,
    role_arn: &str,
    header: &str,
) -> String {
    let created = events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": stage.bus,
            "EventPattern": json!({
                "source": [source],
                "detail-type": [DETAIL_TYPE],
            })
            .to_string(),
        }),
    )
    .await;
    let rule_arn = created["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {case:?}: no RuleArn in {created}"))
        .to_string();
    let answer = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": stage.bus,
            "Targets": [{
                "Id": "stage",
                "Arn": stage.target_arn(),
                "RoleArn": role_arn,
                "HttpParameters": {
                    // The one `*` of the target's ARN is filled by what the event carries at
                    // this path, so the request the delivery makes names the event's own
                    // resource exactly as a client's own request names the one it addressed.
                    "PathParameterValues": ["$.detail.id"],
                    "HeaderParameters": { SOURCE_HEADER: header },
                },
                "DeadLetterConfig": { "Arn": stage.dead_letter_arn },
                "RetryPolicy": {
                    "MaximumRetryAttempts": RETRIES_ALLOWED,
                    "MaximumEventAgeInSeconds": MAX_EVENT_AGE_SECONDS,
                },
            }],
        }),
    )
    .await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "setup {case:?}: expected the target to be accepted, got {answer}"
    );
    rule_arn
}

#[tokio::test]
async fn every_request_at_an_iam_authorized_stage_is_answered_by_the_identity_that_made_it() {
    let router = emulator();
    let stage = deployed(&router, "exec-api-callers").await;

    let rows: &[Row] = &[
        // FIRST, so that every row after it reads a stage this suite has already seen answer:
        // the dial a client makes itself, as a user allowed the invoke on the method's own ARN.
        Row {
            case: "a client's own request at the api's invoke host, as a user allowed the invoke",
            arrival: Arrival::Signed(Address::InvokeHost),
            document: Some(|stage| policy(INVOKE_ACTION, &stage.target_arn())),
            header: "signed-caller-host",
            id: "ord-dialled-1",
            served: true,
        },
        Row {
            case: "the same request at the address this emulator serves the stage at",
            arrival: Arrival::Signed(Address::DataPlanePath),
            document: Some(|stage| policy(INVOKE_ACTION, &stage.target_arn())),
            header: "signed-caller-path",
            id: "ord-dialled-2",
            served: true,
        },
        Row {
            case: "a client's own request as a user allowed only another action there",
            arrival: Arrival::Signed(Address::InvokeHost),
            document: Some(|stage| policy(OTHER_ACTION, &stage.target_arn())),
            header: "signed-caller-other-action",
            id: "ord-dialled-3",
            served: false,
        },
        Row {
            case: "a client's own request as a user holding no document at all",
            arrival: Arrival::Signed(Address::InvokeHost),
            document: None,
            header: "signed-caller-ungranted",
            id: "ord-dialled-4",
            served: false,
        },
        Row {
            case: "a delivery EventBridge makes as a role allowed the invoke on the method's own \
                   ARN",
            arrival: Arrival::Delivered,
            document: Some(|stage| policy(INVOKE_ACTION, &stage.target_arn())),
            header: "rule-good",
            id: "ord-delivered-1",
            served: true,
        },
        Row {
            case: "a delivery EventBridge makes as a role allowed only another action there",
            arrival: Arrival::Delivered,
            document: Some(|stage| policy(OTHER_ACTION, &stage.target_arn())),
            header: "rule-denied",
            id: "ord-delivered-2",
            served: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // What the request carries as its body, which the method's template writes into the sink,
        // so a served row shows WHAT reached the integration.
        let sent = json!({ "id": row.id, "src": row.header });
        let document = row.document.map(|build| build(&stage));
        let mut rule_arn = String::new();
        match row.arrival {
            Arrival::Signed(address) => {
                // The identity a client's own request is made under: one user of this account per
                // row, carrying that row's own document and signing with its own long-term key.
                let user = format!("exec-api-callers-user-{index}");
                let key = signing_user(&router, &user, document.as_deref()).await;
                let (status, text) = match row.served {
                    true => {
                        served_within(
                            &router,
                            &stage.api,
                            address,
                            &key,
                            row.id,
                            row.header,
                            &sent,
                        )
                        .await
                    }
                    false => {
                        signed_post(&router, &stage.api, address, &key, row.id, row.header, &sent)
                            .await
                    }
                };
                match row.served {
                    true => assert_eq!(
                        status, 200,
                        "case {:?}: expected the method's own answer, got {status} with {text:?}",
                        row.case
                    ),
                    false => {
                        assert_eq!(
                            status, 403,
                            "case {:?}: expected the invoke endpoint to refuse this identity, got \
                             {status} with {text:?}",
                            row.case
                        );
                        let message = message_of(&text);
                        assert!(
                            message.contains(NOT_AUTHORIZED_CLAUSE),
                            "case {:?}: expected the refusal to name the action it withheld, got \
                             {message:?}",
                            row.case
                        );
                        assert!(
                            message.contains(&user),
                            "case {:?}: expected the refusal to name the principal the credential \
                             speaks for, got {message:?}",
                            row.case
                        );
                    }
                }
            }
            Arrival::Delivered => {
                let role_arn = role(
                    &router,
                    &format!("exec-api-callers-role-{index}"),
                    EVENTS_TRUST,
                    document.as_deref(),
                )
                .await;
                // One source per row, so this row's publication reaches this row's target and no
                // other row's.
                let source = format!("sim2real.orders.callers.{index}");
                rule_arn = wired(
                    &router,
                    row.case,
                    &stage,
                    &format!("exec-api-callers-{index}"),
                    &source,
                    &role_arn,
                    row.header,
                )
                .await;
                published(&router, row.case, &stage, &source, row.id).await;
            }
        }

        if row.served {
            // The integration behind the method ran: the sink holds the one request, carrying the
            // resource it addressed and what the caller contributed.
            let held = messages(&router, row.case, &stage.sink_url, 1).await;
            assert_eq!(
                held.len(),
                1,
                "case {:?}: expected the one request this identity made at the method, got \
                 {held:?}",
                row.case
            );
            let body = body_of(row.case, &held[0]);
            assert_eq!(
                body["pathId"].as_str(),
                Some(row.id),
                "case {:?}: expected the resource the request addressed, got {body}",
                row.case
            );
            assert_eq!(
                body["orderSrc"].as_str(),
                Some(row.header),
                "case {:?}: expected the header the caller contributed, got {body}",
                row.case
            );
            assert_eq!(
                body["route"].as_str(),
                Some("/orders/{id}"),
                "case {:?}: expected the method's own resource path, got {body}",
                row.case
            );
            let carried = body["delivered"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: expected the body of the request the method served, got \
                         {body}",
                        row.case
                    )
                });
            match row.arrival {
                // A client's own request carries the client's own body.
                Arrival::Signed(_) => assert_eq!(
                    carried, sent,
                    "case {:?}: expected the body the client sent, got {carried}",
                    row.case
                ),
                // A target naming no `InputTransformer` receives the WHOLE envelope of the event
                // that matched, which is what the delivery sends as its body.
                Arrival::Delivered => {
                    assert_eq!(
                        carried["detail-type"].as_str(),
                        Some(DETAIL_TYPE),
                        "case {:?}: expected the event's envelope as the body, got {carried}",
                        row.case
                    );
                    assert_eq!(
                        carried["detail"]["id"].as_str(),
                        Some(row.id),
                        "case {:?}: expected the delivered event to be the one published, got \
                         {carried}",
                        row.case
                    );
                }
            }
            let dead = messages(&router, row.case, &stage.dead_letter_url, 0).await;
            assert!(
                dead.is_empty(),
                "case {:?}: a request the method served is no failed delivery, got {dead:?}",
                row.case
            );
            drained(&router, row.case, &stage.sink_url).await;
            continue;
        }

        // Nothing reached the integration: a request the endpoint refused never got behind it.
        let held = messages(&router, row.case, &stage.sink_url, 0).await;
        assert!(
            held.is_empty(),
            "case {:?}: expected nothing behind a method that served no request, got {held:?}",
            row.case
        );
        // A refusal a CLIENT asked for is answered to the client and is nobody's failed delivery;
        // one EventBridge asked for is recorded on the target's dead-letter queue, because the
        // caller it has to reach is not holding the connection.
        let wanted = usize::from(matches!(row.arrival, Arrival::Delivered));
        let dead = messages(&router, row.case, &stage.dead_letter_url, wanted).await;
        assert_eq!(
            dead.len(),
            wanted,
            "case {:?}: expected {wanted} dead letter(s) for this refusal, got {dead:?}",
            row.case
        );
        if wanted == 0 {
            continue;
        }
        assert_eq!(
            attribute(&dead[0], "ERROR_CODE").as_deref(),
            Some(NO_PERMISSIONS),
            "case {:?}: expected the {NO_PERMISSIONS:?} condition, got {}",
            row.case,
            dead[0]
        );
        assert_forbidden_sentence(
            row.case,
            &attribute(&dead[0], "ERROR_MESSAGE").unwrap_or_default(),
            &format!("/_aws/execute-api/{}/{STAGE}/orders/{}", stage.api, row.id),
        );
        assert_eq!(
            attribute(&dead[0], "TARGET_ARN").as_deref(),
            Some(stage.target_arn().as_str()),
            "case {:?}: expected the target's own ARN on the dead letter, got {}",
            row.case,
            dead[0]
        );
        assert_eq!(
            attribute(&dead[0], "RULE_ARN").as_deref(),
            Some(rule_arn.as_str()),
            "case {:?}: expected the invoking rule's ARN on the dead letter, got {}",
            row.case,
            dead[0]
        );
        let parked = body_of(row.case, &dead[0]);
        assert_eq!(
            parked["detail"]["id"].as_str(),
            Some(row.id),
            "case {:?}: expected the parked event to be the one published, got {parked}",
            row.case
        );
        drained(&router, row.case, &stage.dead_letter_url).await;
    }
}
