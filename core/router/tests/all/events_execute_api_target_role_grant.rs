// SPDX-License-Identifier: CC0-1.0

//! What the GRANT on an EventBridge target's role decides for a target that is one method of a
//! deployed API Gateway REST API stage: which deliveries the stage serves, and what the ones it
//! does not serve leave on the target's dead-letter queue.
//!
//! RULE: EventBridge invokes an API Gateway target AS the role the target names — "for EC2
//! instances, Kinesis data streams, Step Functions state machines and API Gateway APIs,
//! EventBridge relies on IAM roles that you specify in the `RoleARN` argument in `PutTargets`"
//! — so an identity that may not perform `execute-api:Invoke` on the target's OWN ARN is
//! answered `403 Forbidden` by the stage: the delivery fails, nothing reaches what the method
//! integrates with, and the event is written to the target's dead-letter queue with
//! `ERROR_CODE` `NO_PERMISSIONS` and the sentence EventBridge words a status an invoke answered
//! with. The documents in force on the role are read AT EVERY DELIVERY, so the same target
//! delivers once a grant for that action on that ARN is in force.
//!
//! Only a cross-service suite can pin this: the rule, its target and the dead letter are
//! EventBridge's, the stage and its integration are API Gateway's, the role whose documents
//! decide the delivery is IAM's, and the sink the method writes into — which is what shows
//! whether anything reached the stage at all — is Amazon SQS's. The setup calls are the ones
//! [`crate::events_rest_api_stage_target`] already makes for that span, imported rather than
//! written again; what is this suite's own is the bus, the target and the grants below.
//!
//! The rows vary WHICH identity-based grant the role carries, which is what the rule
//! generalises over: another action of the same service on the target's own ARN, the invoke
//! action on another api's methods, no document at all, an explicit deny over an allow, and the
//! grant the delivery needs. The repair test then varies WHEN the grant is written — after the
//! rule is already wired and has failed a delivery.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::events_rest_api_stage_target::{
    apigw, attribute, emulator, events, granted, messages, policy, queue, role, sink_method, sqs,
    APIGATEWAY_TRUST, DETAIL_TYPE, EVENTS_PRINCIPAL, EVENTS_TRUST, NO_PERMISSIONS, REGION,
    SOURCE_HEADER, STAGE,
};

/// The `source` the events of one row carry, and what that row's own rule matches on: each row
/// publishes under a source of its own, so a row's publication reaches that row's target and no
/// other row's.
fn source_of(index: usize) -> String {
    format!("sim2real.orders.{index}")
}

/// The value the target contributes as [`SOURCE_HEADER`], which is what the sink shows about a
/// delivery the stage served.
const SOURCE_HEADER_VALUE: &str = "rule-denied";

/// The action the delivery performs, which the target's role has to be allowed on the target's
/// own ARN.
///
/// This suite's own setup is the crate's one setup for a CUSTOM BUS whose rules target a deployed
/// stage: the sibling suite that varies the FAN-OUT of one event over several such targets
/// (`crate::events_execute_api_target_role_independence`) imports the items marked `pub(crate)`
/// here, so there is ONE set of setup calls and ONE wording of the refused invoke for both.
pub(crate) const INVOKE_ACTION: &str = "execute-api:Invoke";

/// The opening of the sentence EventBridge words a refused invoke with: the status the stage
/// answered, and the request it answered it to.
const FORBIDDEN_OPENING: &str = r#"Got status code "403" while trying to invoke "POST "#;

/// The clause that closes it, up to the per-delivery request id: the service the request was
/// made to, the status and the code the stage's answer carried.
const FORBIDDEN_CLAUSE: &str =
    "\". (Service: api_gateway; Status Code: 403; Error Code: Forbidden; Request ID: ";

/// The last clause of every such sentence.
const FORBIDDEN_TAIL: &str = "; Proxy: null)";

/// How many retries the target allows, which a delivery no request is served for makes none of.
pub(crate) const RETRIES_ALLOWED: i64 = 0;

/// The oldest an event the target still accepts may be, the modelled minimum
/// (`RetryPolicy$MaximumEventAgeInSeconds`, `@range(min: 60)`).
pub(crate) const MAX_EVENT_AGE_SECONDS: i64 = 60;

/// One identity-based policy that ALLOWS `action` on `resource` and explicitly denies it there
/// as well, which is the pair an explicit deny decides.
pub(crate) fn allow_and_deny(action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"{action}","Resource":"{resource}"}},{{"Effect":"Deny","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// Drain `url` of everything it holds, so the next row of a table reads only its own messages.
pub(crate) async fn drained(router: &MultiServiceRouter, case: &str, url: &str) {
    for message in messages(router, case, url, 0).await {
        sqs(
            router,
            case,
            "DeleteMessage",
            &json!({
                "QueueUrl": url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        )
        .await;
    }
}

/// One stage this suite deploys, and everything the rows address it by.
pub(crate) struct Deployed {
    pub(crate) api: String,
    pub(crate) account: String,
    pub(crate) bus: String,
    pub(crate) sink_url: String,
    pub(crate) dead_letter_url: String,
    pub(crate) dead_letter_arn: String,
}

impl Deployed {
    /// The ARN a target names the method of this stage by: every `POST` under `/orders`, whose
    /// one `*` the target's own path parameter fills.
    pub(crate) fn target_arn(&self) -> String {
        format!(
            "arn:aws:execute-api:{REGION}:{}:{}/{STAGE}/POST/orders/*",
            self.account, self.api
        )
    }
}

/// The custom bus, the queues, the api and its deployed stage every row below uses.
pub(crate) async fn deployed(router: &MultiServiceRouter, name: &str) -> Deployed {
    let (sink_url, sink_arn) = queue(router, "sink", &format!("{name}-sink")).await;
    let (dead_letter_url, dead_letter_arn) =
        queue(router, "dead letter", &format!("{name}-dlq")).await;
    let account = sink_arn.split(':').nth(4).unwrap_or_default().to_string();
    // The dead-letter queue's own policy admits the write EventBridge makes as its own service
    // principal ("for … Amazon SQS queues, EventBridge relies on resource-based policies"), so
    // what reaches it is decided by the target's delivery and not by this queue.
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
    let sink_uri = format!(
        "arn:aws:apigateway:{REGION}:sqs:path/{account}/{}",
        sink_url.rsplit('/').next().unwrap_or_default()
    );
    // The method the rules deliver to is authorized by IAM, which is what makes the request
    // EventBridge signs the only one it serves.
    sink_method(router, &api, &root, "orders", "AWS_IAM", &sink_uri, &sender).await;
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    let bus = format!("{name}-bus");
    events(router, "bus", "CreateEventBus", &json!({ "Name": bus })).await;

    Deployed {
        api,
        account,
        bus,
        sink_url,
        dead_letter_url,
        dead_letter_arn,
    }
}

/// Wire one rule of the custom bus to the stage target, as `role_arn`, and answer the rule's
/// own ARN.
async fn wired(
    router: &MultiServiceRouter,
    case: &str,
    stage: &Deployed,
    name: &str,
    source: &str,
    role_arn: &str,
) -> String {
    let rule = events(
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
    let rule_arn = rule["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {case:?}: no RuleArn in {rule}"))
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
                    // this path, so one rule addresses a different resource per event.
                    "PathParameterValues": ["$.detail.id"],
                    "HeaderParameters": { SOURCE_HEADER: SOURCE_HEADER_VALUE },
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

/// Publish one matching entry on the custom bus, which has to be accepted.
async fn published(
    router: &MultiServiceRouter,
    case: &str,
    stage: &Deployed,
    source: &str,
    id: &str,
) {
    let answer = events(
        router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": stage.bus,
                "Source": source,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "id": id, "src": "runner" }).to_string(),
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

/// Assert that `message` is the sentence EventBridge words a `403` answer with, for a request at
/// `path`, answering the id that sentence names the answered request by.
pub(crate) fn assert_forbidden_sentence(case: &str, message: &str, path: &str) -> String {
    assert!(
        message.starts_with(FORBIDDEN_OPENING),
        "case {case:?}: expected a message opening {FORBIDDEN_OPENING:?}, got {message:?}"
    );
    let clause = format!("{path}{FORBIDDEN_CLAUSE}");
    assert!(
        message.contains(&clause),
        "case {case:?}: expected a message naming the request and its answer as {clause:?}, \
         got {message:?}"
    );
    assert!(
        message.ends_with(FORBIDDEN_TAIL),
        "case {case:?}: expected a message closing {FORBIDDEN_TAIL:?}, got {message:?}"
    );
    let request_id = message
        .rsplit_once(FORBIDDEN_CLAUSE)
        .map(|(_, tail)| tail.trim_end_matches(FORBIDDEN_TAIL).to_string())
        .unwrap_or_default();
    assert!(
        !request_id.is_empty() && !request_id.contains(' '),
        "case {case:?}: expected the sentence to name the request id of the answer, got \
         {message:?}"
    );
    request_id
}

/// One row of the grant table: which document is in force on the target's role, and whether the
/// stage then serves the delivery.
struct Grant {
    case: &'static str,
    /// The document written as the role's inline policy, built from the target's own ARN —
    /// `None` for a role carrying no identity-based document at all.
    document: fn(&Deployed) -> Option<String>,
    /// Whether the stage serves the delivery, which is what the sink shows.
    served: bool,
}

#[tokio::test]
async fn an_execute_api_target_is_delivered_to_only_under_a_role_granted_the_invoke() {
    let router = emulator();
    let stage = deployed(&router, "exec-api-grant").await;
    let grants: &[Grant] = &[
        Grant {
            case: "a role allowed another action of the same service on the target's own ARN",
            document: |stage| Some(policy("execute-api:InvalidateCache", &stage.target_arn())),
            served: false,
        },
        Grant {
            case: "a role allowed the invoke on another api's methods",
            document: |stage| {
                Some(policy(
                    INVOKE_ACTION,
                    &format!(
                        "arn:aws:execute-api:{REGION}:{}:zzz9999999/{STAGE}/POST/orders/*",
                        stage.account
                    ),
                ))
            },
            served: false,
        },
        Grant {
            case: "a role carrying no identity-based document at all",
            document: |_stage| None,
            served: false,
        },
        Grant {
            case: "a role whose document denies the invoke it also allows",
            document: |stage| Some(allow_and_deny(INVOKE_ACTION, &stage.target_arn())),
            served: false,
        },
        Grant {
            case: "a role allowed the invoke on the target's own ARN",
            document: |stage| Some(policy(INVOKE_ACTION, &stage.target_arn())),
            served: true,
        },
    ];

    for (index, grant) in grants.iter().enumerate() {
        let document = (grant.document)(&stage);
        let role_arn = role(
            &router,
            &format!("exec-api-grant-role-{index}"),
            EVENTS_TRUST,
            document.as_deref(),
        )
        .await;
        let source = source_of(index);
        let rule_arn = wired(
            &router,
            grant.case,
            &stage,
            &format!("exec-api-grant-{index}"),
            &source,
            &role_arn,
        )
        .await;
        let id = format!("ord-{index}");
        published(&router, grant.case, &stage, &source, &id).await;

        if grant.served {
            let held = messages(&router, grant.case, &stage.sink_url, 1).await;
            assert_eq!(
                held.len(),
                1,
                "case {:?}: expected the one request this delivery made at the stage, got \
                 {held:?}",
                grant.case
            );
            let body: Value = held[0]["Body"]
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: expected the document the stage's template wrote, got {}",
                        grant.case, held[0]
                    )
                });
            assert_eq!(
                body["pathId"].as_str(),
                Some(id.as_str()),
                "case {:?}: expected the path the event names, got {body}",
                grant.case
            );
            assert_eq!(
                body["orderSrc"].as_str(),
                Some(SOURCE_HEADER_VALUE),
                "case {:?}: expected the header the target contributes, got {body}",
                grant.case
            );
            let dead = messages(&router, grant.case, &stage.dead_letter_url, 0).await;
            assert!(
                dead.is_empty(),
                "case {:?}: a delivery the stage served dead-letters nothing, got {dead:?}",
                grant.case
            );
            drained(&router, grant.case, &stage.sink_url).await;
            continue;
        }

        let dead = messages(&router, grant.case, &stage.dead_letter_url, 1).await;
        assert_eq!(
            dead.len(),
            1,
            "case {:?}: expected the one dead letter this refused delivery produced, got \
             {dead:?}",
            grant.case
        );
        assert_eq!(
            attribute(&dead[0], "ERROR_CODE").as_deref(),
            Some(NO_PERMISSIONS),
            "case {:?}: expected the {NO_PERMISSIONS:?} condition, got {}",
            grant.case,
            dead[0]
        );
        assert_forbidden_sentence(
            grant.case,
            &attribute(&dead[0], "ERROR_MESSAGE").unwrap_or_default(),
            &format!("/_aws/execute-api/{}/{STAGE}/orders/{id}", stage.api),
        );
        assert_eq!(
            attribute(&dead[0], "TARGET_ARN").as_deref(),
            Some(stage.target_arn().as_str()),
            "case {:?}: expected the target's own ARN on the dead letter, got {}",
            grant.case,
            dead[0]
        );
        assert_eq!(
            attribute(&dead[0], "RULE_ARN").as_deref(),
            Some(rule_arn.as_str()),
            "case {:?}: expected the invoking rule's ARN on the dead letter, got {}",
            grant.case,
            dead[0]
        );
        // The event itself is what is parked, so a consumer of the queue has the delivery that
        // failed as well as why it failed.
        let parked: Value = dead[0]["Body"]
            .as_str()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the published event as the dead letter's body, got {}",
                    grant.case, dead[0]
                )
            });
        assert_eq!(
            parked["detail"]["id"].as_str(),
            Some(id.as_str()),
            "case {:?}: expected the parked event to be the one published, got {parked}",
            grant.case
        );
        let held = messages(&router, grant.case, &stage.sink_url, 0).await;
        assert!(
            held.is_empty(),
            "case {:?}: expected nothing behind a stage that served no request, got {held:?}",
            grant.case
        );
        drained(&router, grant.case, &stage.dead_letter_url).await;
    }
}

#[tokio::test]
async fn the_grant_written_after_the_rule_decides_the_next_delivery() {
    let case = "a grant written after the target has already failed a delivery";
    let router = emulator();
    let stage = deployed(&router, "exec-api-repair").await;
    let name = "exec-api-repair-role";
    let role_arn = role(
        &router,
        name,
        EVENTS_TRUST,
        Some(&policy("execute-api:InvalidateCache", &stage.target_arn())),
    )
    .await;
    let source = source_of(0);
    wired(&router, case, &stage, "exec-api-repair", &source, &role_arn).await;

    published(&router, case, &stage, &source, "ord-before").await;
    let dead = messages(&router, case, &stage.dead_letter_url, 1).await;
    assert_eq!(
        dead.len(),
        1,
        "case {case:?}: expected the refused delivery's dead letter, got {dead:?}"
    );
    assert_eq!(
        attribute(&dead[0], "ERROR_CODE").as_deref(),
        Some(NO_PERMISSIONS),
        "case {case:?}: expected the {NO_PERMISSIONS:?} condition, got {}",
        dead[0]
    );

    // The documents in force on the role are read at every delivery, so the grant written now
    // decides the next one — the target, the rule and the stage are untouched.
    granted(&router, name, &policy(INVOKE_ACTION, &stage.target_arn())).await;
    published(&router, case, &stage, &source, "ord-after").await;
    let held = messages(&router, case, &stage.sink_url, 1).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected the granted delivery's one request at the stage, got {held:?}"
    );
    let body: Value = held[0]["Body"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the document the stage's template wrote, got {}",
                held[0]
            )
        });
    assert_eq!(
        body["pathId"].as_str(),
        Some("ord-after"),
        "case {case:?}: expected the path the second event names, got {body}"
    );
}
