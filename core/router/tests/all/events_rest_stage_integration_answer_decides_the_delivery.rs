// SPDX-License-Identifier: CC0-1.0

//! What a deployed API Gateway REST API stage answers a `POST` at one of its methods, and what an
//! EventBridge rule whose target is that method then comes to.
//!
//! RULE: a request to a method of a DEPLOYED stage is answered FROM THAT METHOD'S INTEGRATION —
//! the status and the body the integration produces, whichever class of integration it is — and
//! the gateway's own server-error answer is served only for an integration the stage cannot
//! perform. A rule whose `EventPattern` matched hands the event to such a target BY MAKING THAT
//! REQUEST, so the stage's answer is what decides the delivery: a method that serves the request
//! takes the event and dead-letters nothing, while one answering 5xx is retried within the
//! target's `RetryPolicy` and then written to the target's dead-letter queue, carrying the
//! delivery's own error code, a message naming the status the stage answered and the request it
//! answered, and the number of retries that bound allowed.
//!
//! Only a cross-service suite can pin this: the stage, its methods and the integration each one
//! is answered from are API Gateway's, the roles the stage and the delivery act as are IAM's, the
//! rule, its target and the dead letter are EventBridge's, and the sink one of those integrations
//! writes into — which is what shows WHAT reached the integration rather than what the stage said
//! about it — is Amazon SQS's. The stage this suite stands up is its own, because what it varies
//! is the INTEGRATION CLASS behind the method; the setup calls, the queues and the polling are
//! the ones [`crate::events_rest_api_stage_target`] already spells, imported rather than written
//! again.
//!
//! The rows vary what the rule generalises over: the class of integration the method carries (one
//! answered from the api's own records alone, one performed as a call to another service, and one
//! the stage cannot perform at all), and — for the client's own control dial — the addressing the
//! request arrives by (the api's own invoke host, and the address this emulator serves every
//! stage at).

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::events_execute_api_target_role_grant::{drained, INVOKE_ACTION};
use crate::events_rest_api_stage_target::{
    apigw, attribute, emulator, events, messages, policy, queue, resource, role, sink_method, sqs,
    APIGATEWAY_TRUST, DETAIL_TYPE, EVENTS_PRINCIPAL, EVENTS_TRUST, JSON, REGION, SOURCE_HEADER,
    STAGE,
};

/// The first path segment every method below lives under, so each row's method is a NESTED
/// resource of the api's tree and not a child of its root.
const PREFIX: &str = "v1";

/// The condition EventBridge names on the dead letter of a delivery whose endpoint did not answer
/// the request successfully — here the stage's own 5xx answer, whose status the message beside it
/// carries. Spelled here rather than imported from the service, so a wrong value in the service
/// would fail this suite.
const CONNECTION_FAILURE: &str = "CONNECTION_FAILURE";

/// The dead-letter attribute carrying EventBridge's own classification of the failure, spelled
/// here for the same reason.
const ERROR_CODE: &str = "ERROR_CODE";

/// The one carrying the sentence that classification is worded in.
const ERROR_MESSAGE: &str = "ERROR_MESSAGE";

/// The one saying how many RETRIES the delivery made.
const RETRY_ATTEMPTS: &str = "RETRY_ATTEMPTS";

/// How many retries every target below allows, which is what the dead letter of a retried
/// delivery has to report.
const RETRIES_ALLOWED: i64 = 2;

/// The sentence API Gateway's default 5xx gateway response carries, which is what a stage answers
/// for a request whose integration it cannot perform — and what the stage must NOT answer for a
/// method whose integration it can.
const SERVER_ERROR: &str = "Internal server error";

/// The document the `MOCK` method's integration response renders, which is what a client of that
/// method is served: an integration whose answer is the api's own records and nothing else.
const MOCK_ANSWER: &str = r#"{"accepted":true}"#;

/// The integration request template of the `MOCK` method: the `statusCode` the rendered template
/// names is the integration response the call maps to
/// (`smithy_query(query="operation",operation="PutIntegration")` for `requestTemplates`).
const MOCK_TEMPLATE: &str = r#"{"statusCode": 200}"#;

/// What a client's own control dial contributes as the header the stage records.
const CONTROL_HEADER: &str = "control-dial";

/// How long a dial of the stage keeps being retried before this suite gives up on it: generous,
/// because what it waits for is a CONDITION — a stage that is deployed answers from its method —
/// and never a duration.
const DIAL_DEADLINE: Duration = Duration::from_secs(30);

/// How long such a dial waits before looking again.
const DIAL_INTERVAL: Duration = Duration::from_millis(20);

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

    /// The path the request addresses `POST /{PREFIX}/{part}/{{id}}` of the stage by.
    fn path(self, api: &str, part: &str, id: &str) -> String {
        match self {
            Self::InvokeHost => format!("/{STAGE}/{PREFIX}/{part}/{id}"),
            Self::DataPlanePath => {
                format!("/_aws/execute-api/{api}/{STAGE}/{PREFIX}/{part}/{id}")
            }
        }
    }

    /// How a case names this addressing.
    fn case(self) -> &'static str {
        match self {
            Self::InvokeHost => "the api's own invoke host",
            Self::DataPlanePath => "the address this emulator serves the stage at",
        }
    }

    /// What the resource a dial at this addressing addresses is named after, so the two dials of
    /// one row address two different resources of the same method.
    fn slug(self) -> &'static str {
        match self {
            Self::InvokeHost => "host",
            Self::DataPlanePath => "path",
        }
    }
}

/// Both addressings of one stage, which every row's control dial is made at.
const ADDRESSINGS: [Address; 2] = [Address::InvokeHost, Address::DataPlanePath];

/// Dial `POST /{PREFIX}/{part}/{id}` of the deployed stage once, as a client of a `NONE` method —
/// which authenticates nobody, so the dial carries no credential — and answer the status and the
/// body that came back.
async fn dial(
    router: &MultiServiceRouter,
    api: &str,
    address: Address,
    part: &str,
    id: &str,
) -> (u16, String) {
    let host = address.host(api);
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}{}", address.path(api, part, id)))
        .header("host", host)
        .header("content-type", JSON)
        .header(SOURCE_HEADER, CONTROL_HEADER)
        .body(bytes::Bytes::from(
            json!({ "id": id, "src": CONTROL_HEADER }).to_string(),
        ))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(&response.into_body()).to_string(),
    )
}

/// Dial the stage until it answers `expected`, or [`DIAL_DEADLINE`] passes — the one condition
/// this suite waits on at the stage, so no row depends on how fast the host runs.
async fn dialled_within(
    router: &MultiServiceRouter,
    api: &str,
    address: Address,
    part: &str,
    id: &str,
    expected: u16,
) -> (u16, String) {
    let deadline = Instant::now() + DIAL_DEADLINE;
    loop {
        let answered = dial(router, api, address, part, id).await;
        if answered.0 == expected || Instant::now() >= deadline {
            return answered;
        }
        ::tokio::time::sleep(DIAL_INTERVAL).await;
    }
}

/// The document a message carries as its body, which for a message at the sink is the one the
/// method's integration template wrote out of the request the method served.
fn body_of(case: &str, message: &Value) -> Value {
    message["Body"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a document as the body, got {message}"))
}

/// One stage this suite deploys, and everything the rows address it by.
struct Deployed {
    api: String,
    account: String,
    sink_url: String,
    dead_letter_url: String,
    dead_letter_arn: String,
    /// A role EventBridge can assume that may invoke every `POST` method of this stage.
    invoker: String,
}

impl Deployed {
    /// The ARN a target names one method of this stage by: every `POST` under
    /// `/{PREFIX}/{part}`, whose one `*` the target's own path parameter fills.
    fn target_arn(&self, part: &str) -> String {
        format!(
            "arn:aws:execute-api:{REGION}:{}:{}/{STAGE}/POST/{PREFIX}/{part}/*",
            self.account, self.api
        )
    }
}

/// A `POST /{part}/{{id}}` method under `parent` whose `MOCK` integration answers
/// [`MOCK_ANSWER`]: the one integration class that needs no backend at all, performed in full
/// from the api's own records.
async fn mock_method(router: &MultiServiceRouter, api: &str, parent: &str, part: &str) {
    let holder = resource(router, api, parent, part).await;
    let child = resource(router, api, &holder, "{id}").await;
    let case = format!("MOCK POST /{part}/{{id}}");
    let method = format!("/restapis/{api}/resources/{child}/methods/POST");
    apigw(
        router,
        &case,
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        &case,
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { JSON: MOCK_TEMPLATE },
        })),
        201,
    )
    .await;
    apigw(
        router,
        &case,
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw(
        router,
        &case,
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({
            "statusCode": "200",
            "responseTemplates": { JSON: MOCK_ANSWER },
        })),
        201,
    )
    .await;
}

/// The queues, the roles, the api and its deployed stage every row below uses.
///
/// Three methods under `/{PREFIX}`, one per integration class the rows span: `accepted`, a `MOCK`
/// answered from the api's own records; `sunk`, an `AWS` integration that SENDS the request to a
/// queue as a role the api can assume; and `broken`, the same integration named under a role it
/// cannot assume — an integration the stage cannot perform, which it answers through its own 5xx
/// gateway response.
async fn deployed(router: &MultiServiceRouter) -> Deployed {
    let name = "rest-stage-integration-classes";
    let (sink_url, sink_arn) = queue(router, "sink", &format!("{name}-sink")).await;
    let (dead_letter_url, dead_letter_arn) =
        queue(router, "dead letter", &format!("{name}-dlq")).await;
    let account = sink_arn.split(':').nth(4).unwrap_or_default().to_string();
    // The dead-letter queue's own policy admits the write EventBridge makes as its own service
    // principal, so what reaches it is decided by the target's delivery and not by this queue.
    sqs(
        router,
        "dead letter",
        "SetQueueAttributes",
        &json!({
            "QueueUrl": dead_letter_url,
            "Attributes": {
                "Policy": json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": dead_letter_arn,
                    }],
                })
                .to_string(),
            },
        }),
    )
    .await;

    let sender = role(
        router,
        &format!("{name}-sender"),
        APIGATEWAY_TRUST,
        Some(&policy("sqs:SendMessage", &sink_arn)),
    )
    .await;
    // A role the api cannot assume at all, which is what makes the `broken` method's integration
    // one the stage cannot perform.
    let unassumable = role(
        router,
        &format!("{name}-unassumable"),
        EVENTS_TRUST,
        Some(&policy("sqs:SendMessage", &sink_arn)),
    )
    .await;

    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": name })),
        201,
    )
    .await;
    let api = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateRestApi: no id in {created}"))
        .to_string();
    let root = created["rootResourceId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateRestApi: no rootResourceId in {created}"))
        .to_string();
    let holder = resource(router, &api, &root, PREFIX).await;
    let sink_uri = format!(
        "arn:aws:apigateway:{REGION}:sqs:path/{account}/{}",
        sink_url.rsplit('/').next().unwrap_or_default()
    );
    mock_method(router, &api, &holder, "accepted").await;
    sink_method(router, &api, &holder, "sunk", "NONE", &sink_uri, &sender).await;
    sink_method(
        router,
        &api,
        &holder,
        "broken",
        "NONE",
        &sink_uri,
        &unassumable,
    )
    .await;
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    let invoker = role(
        router,
        &format!("{name}-invoker"),
        EVENTS_TRUST,
        Some(&policy(
            INVOKE_ACTION,
            &format!("arn:aws:execute-api:{REGION}:{account}:{api}/{STAGE}/POST/*"),
        )),
    )
    .await;
    Deployed {
        api,
        account,
        sink_url,
        dead_letter_url,
        dead_letter_arn,
        invoker,
    }
}

/// Wire one rule of the default bus to the `POST` method under `/{PREFIX}/{part}`, as a role
/// allowed the invoke, dead-lettering to this stage's queue within [`RETRIES_ALLOWED`] retries.
async fn wired(
    router: &MultiServiceRouter,
    case: &str,
    stage: &Deployed,
    name: &str,
    source: &str,
    part: &str,
) {
    events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventPattern": json!({
                "source": [source],
                "detail-type": [DETAIL_TYPE],
            })
            .to_string(),
        }),
    )
    .await;
    let answer = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "Targets": [{
                "Id": "stage",
                "Arn": stage.target_arn(part),
                "RoleArn": stage.invoker,
                "HttpParameters": {
                    // The one `*` of the target's ARN is filled by what the event carries at
                    // this path, so one rule addresses a different resource per event.
                    "PathParameterValues": ["$.detail.id"],
                    "HeaderParameters": { SOURCE_HEADER: "$.detail.src" },
                },
                "DeadLetterConfig": { "Arn": stage.dead_letter_arn },
                "RetryPolicy": { "MaximumRetryAttempts": RETRIES_ALLOWED },
            }],
        }),
    )
    .await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "setup {case:?}: expected the target to be accepted, got {answer}"
    );
}

/// Publish one entry on the default bus, which has to be accepted.
async fn published(router: &MultiServiceRouter, case: &str, source: &str, id: &str, src: &str) {
    let answer = events(
        router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": source,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "id": id, "src": src }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the entry to be accepted, got {answer}"
    );
}

/// What the dead letter of a delivery to one of these methods carries.
struct DeadLettered {
    /// The condition EventBridge names.
    code: &'static str,
    /// What its sentence has to carry, whose variable part is the status the stage answered.
    message: &'static str,
    /// How many retries it reports, `None` where the delivery retried nothing.
    retries: Option<i64>,
}

/// One row: the integration class the method under `/{PREFIX}/{part}` carries, what the stage
/// answers a client's own `POST` there, and what a delivery EventBridge makes to it comes to.
struct Row {
    case: &'static str,
    part: &'static str,
    /// The status the stage answers, which is the integration's own.
    status: u16,
    /// The body it answers verbatim, `None` where the answer is a document this suite asserts
    /// nothing about the wording of — the one another service produced, or the gateway's own
    /// server-error response, which is asserted by its sentence instead.
    body: Option<&'static str>,
    /// Whether performing the integration writes to the sink, which is what shows the request
    /// reached it.
    writes_to_the_sink: bool,
    /// What a delivery to this method is dead-lettered with, `None` for a method that serves it.
    dead_letter: Option<DeadLettered>,
}

#[tokio::test]
async fn a_rest_stage_answers_each_integration_class_and_the_answer_decides_the_delivery() {
    let router = emulator();
    let stage = deployed(&router).await;

    let rows: &[Row] = &[
        Row {
            case: "a MOCK integration, answered from the api's own records",
            part: "accepted",
            status: 200,
            body: Some(MOCK_ANSWER),
            writes_to_the_sink: false,
            dead_letter: None,
        },
        Row {
            case: "an AWS integration performed as a call to another service",
            part: "sunk",
            status: 200,
            body: None,
            writes_to_the_sink: true,
            dead_letter: None,
        },
        Row {
            case: "an AWS integration under a role the api cannot assume, which the stage cannot \
                   perform",
            part: "broken",
            status: 500,
            body: None,
            writes_to_the_sink: false,
            dead_letter: Some(DeadLettered {
                code: CONNECTION_FAILURE,
                message: r#"Got status code "500" while trying to invoke "POST "#,
                retries: Some(RETRIES_ALLOWED),
            }),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // THE CONTROL DIAL a client makes itself, at both addressings of the stage: what the
        // method's own integration answers, and never the server-error response for an
        // integration the stage can perform.
        for address in ADDRESSINGS {
            let case = format!("{}, dialled at {}", row.case, address.case());
            let id = format!("dialled-{index}-{}", address.slug());
            let (status, text) =
                dialled_within(&router, &stage.api, address, row.part, &id, row.status).await;
            assert_eq!(
                status, row.status,
                "case {case:?}: expected the integration's own {} at the stage, got {status} \
                 with {text:?}",
                row.status
            );
            if let Some(expected) = row.body {
                assert_eq!(
                    text.trim(),
                    expected,
                    "case {case:?}: expected the body the integration answers, got {text:?}"
                );
            }
            match row.status {
                // The answer for an integration the stage cannot perform is its own gateway
                // response, carrying that response's sentence.
                500 => assert!(
                    text.contains(SERVER_ERROR),
                    "case {case:?}: expected the gateway response's own {SERVER_ERROR:?}, got \
                     {text:?}"
                ),
                // And a method whose integration it CAN perform is answered from that
                // integration, never from that response.
                _ => assert!(
                    !text.contains(SERVER_ERROR),
                    "case {case:?}: a method whose integration the stage performs is answered \
                     from it and never with {SERVER_ERROR:?}, got {text:?}"
                ),
            }
            let wanted = usize::from(row.writes_to_the_sink);
            let held = messages(&router, &case, &stage.sink_url, wanted).await;
            assert_eq!(
                held.len(),
                wanted,
                "case {case:?}: expected {wanted} message(s) behind the method, got {held:?}"
            );
            if wanted > 0 {
                let body = body_of(&case, &held[0]);
                assert_eq!(
                    body["pathId"].as_str(),
                    Some(id.as_str()),
                    "case {case:?}: expected the resource the dial addressed, got {body}"
                );
                assert_eq!(
                    body["route"].as_str(),
                    Some(format!("/{PREFIX}/{}/{{id}}", row.part).as_str()),
                    "case {case:?}: expected the method's own resource path, got {body}"
                );
                assert_eq!(
                    body["orderSrc"].as_str(),
                    Some(CONTROL_HEADER),
                    "case {case:?}: expected the header the client contributed, got {body}"
                );
            }
            drained(&router, &case, &stage.sink_url).await;
        }

        // THE DELIVERY EventBridge makes for a matched event, which is the same request made by
        // the rule's target: the stage's answer is what decides it.
        let source = format!("sim2real.orders.classes.{index}");
        wired(
            &router,
            row.case,
            &stage,
            &format!("rest-stage-classes-{index}"),
            &source,
            row.part,
        )
        .await;
        let delivered_id = format!("delivered-{index}");
        let delivered_src = format!("rule-{index}");
        published(&router, row.case, &source, &delivered_id, &delivered_src).await;

        let Some(dead_letter) = row.dead_letter.as_ref() else {
            // A delivery the stage served: the integration behind the method ran — which the
            // sink shows for the method that writes into it — and nothing was dead-lettered.
            if row.writes_to_the_sink {
                let held = messages(&router, row.case, &stage.sink_url, 1).await;
                assert_eq!(
                    held.len(),
                    1,
                    "case {:?}: expected the one request the delivery made at the method, got \
                     {held:?}",
                    row.case
                );
                let body = body_of(row.case, &held[0]);
                assert_eq!(
                    body["pathId"].as_str(),
                    Some(delivered_id.as_str()),
                    "case {:?}: expected the resource the event names, got {body}",
                    row.case
                );
                assert_eq!(
                    body["orderSrc"].as_str(),
                    Some(delivered_src.as_str()),
                    "case {:?}: expected the header the event names, got {body}",
                    row.case
                );
                drained(&router, row.case, &stage.sink_url).await;
            }
            let dead = messages(&router, row.case, &stage.dead_letter_url, 0).await;
            assert!(
                dead.is_empty(),
                "case {:?}: a delivery the stage served dead-letters nothing, got {dead:?}",
                row.case
            );
            continue;
        };

        let dead = messages(&router, row.case, &stage.dead_letter_url, 1).await;
        assert_eq!(
            dead.len(),
            1,
            "case {:?}: expected the one dead letter this delivery produced, got {dead:?}",
            row.case
        );
        assert_eq!(
            attribute(&dead[0], ERROR_CODE).as_deref(),
            Some(dead_letter.code),
            "case {:?}: expected the {:?} condition, got {}",
            row.case,
            dead_letter.code,
            dead[0]
        );
        let message = attribute(&dead[0], ERROR_MESSAGE).unwrap_or_default();
        assert!(
            message.contains(dead_letter.message),
            "case {:?}: expected a message carrying {:?}, got {message:?}",
            row.case,
            dead_letter.message
        );
        // The request the delivery made, which is the stage at the address this emulator serves
        // it at, carrying the resource the event named.
        let addressed = format!(
            "/_aws/execute-api/{}/{STAGE}/{PREFIX}/{}/{delivered_id}",
            stage.api, row.part
        );
        assert!(
            message.contains(&addressed),
            "case {:?}: expected the message to name the request it answered, {addressed:?}, got \
             {message:?}",
            row.case
        );
        let reported =
            attribute(&dead[0], RETRY_ATTEMPTS).and_then(|value| value.parse::<i64>().ok());
        assert_eq!(
            reported, dead_letter.retries,
            "case {:?}: expected {:?} retries reported, got {reported:?}",
            row.case, dead_letter.retries
        );
        let parked = body_of(row.case, &dead[0]);
        assert_eq!(
            parked["detail"]["id"].as_str(),
            Some(delivered_id.as_str()),
            "case {:?}: expected the parked event to be the one published, got {parked}",
            row.case
        );
        // Nothing reached the integration: a delivery the stage answered 5xx got nowhere behind
        // the method.
        let held = messages(&router, row.case, &stage.sink_url, 0).await;
        assert!(
            held.is_empty(),
            "case {:?}: expected nothing behind a method that served no request, got {held:?}",
            row.case
        );
        drained(&router, row.case, &stage.dead_letter_url).await;
    }
}
