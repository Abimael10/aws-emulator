// SPDX-License-Identifier: CC0-1.0

//! A backup plan's MUTATIONS reach an EventBridge rule's Amazon SQS target.
//!
//! The RULE pinned here: **every mutation of an AWS Backup plan that FOLLOWS the call
//! which created it — each update that stores another version of its body, and the delete
//! that records its deletion — is published once onto the account's DEFAULT EventBridge
//! bus as `source` `aws.backup` / `detail-type` `Backup Plan State Change`, whose
//! `detail` names the plan the mutation was performed on (`backupPlanId`, with the plan's
//! other stored members beside it) and the transition it made (`state`: `MODIFIED`,
//! `DELETED`), and carries that plan in `resources`; EventBridge routes the envelope by
//! its CONTENT, so a rule whose `EventPattern` matches hands it UNCHANGED to every one of
//! that rule's targets, and a queue target receives it where that QUEUE's own resource
//! policy admits `sqs:SendMessage` to the `events.amazonaws.com` principal ("for Lambda
//! functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on
//! resource-based policies", `PutTargets`). The plan a CREATE stored is not such a
//! mutation — it is what that call's own response reports — so a rule installed before a
//! create is handed its first envelope for that plan by the mutation that FOLLOWS it.**
//! Nothing else of this service has a detail-type of its own here: an audit framework's
//! create and update publish no envelope at all.
//!
//! Source for the envelope: the AWS Backup EventBridge notifications, which carry
//! `"source": "aws.backup"` with `"detail-type": "Backup Plan State Change"` for a plan
//! that is modified or deleted. The Backup Smithy model describes the control
//! plane, not the bus (`smithy_query(query="operation", operation="UpdateBackupPlan")`
//! models only the synchronous response), so every `detail` member asserted here is
//! asserted against what the SAME mutation's control-plane response reported for it,
//! never against a value this suite put there.
//!
//! Three crates have to agree for the rule to hold — AWS Backup publishes the mutation,
//! EventBridge matches the envelope against each installed pattern and hands it over, and
//! Amazon SQS holds what was handed to it — so the suite lives here rather than in any one
//! of them.
//!
//! The rows differ along exactly what the rule generalises over: WHICH clauses the
//! installed pattern filters on (the source alone; the source and the plan's detail-type;
//! the source and a LIST of detail-types; the source and a detail-type that is another
//! record's; the source, the detail-type and ONE `detail.state`), and therefore which of
//! the plan's envelopes that rule is owed; the SHAPE of the plan each mutation stores (one
//! rule, two rules, advanced backup settings beside the rules); and HOW MANY mutations the
//! plan is given (a create alone, a create and one update, a create and two updates, each
//! with and without the delete). Every row reads its own rule's queue beside a CONTROL
//! rule matching the source alone: the control's envelopes are what say the publication
//! really happened, so a row whose pattern is owed nothing is a refusal to deliver rather
//! than a look taken too early. The create's own silence is read the same way — the
//! control queue is drained right after the create, before any later mutation, and the
//! publication is made by the mutating call itself, so an envelope published for the
//! create would already be waiting on it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// One SigV4 `Authorization` whose credential scope names the service a request is routed
// to, written ONCE for this test crate rather than per suite. It signs for the Region
// below, which is why this suite works in it.
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request below is signed for — the one the shared credential names.
const REGION: &str = "us-east-1";

/// The `source` and `detail-type` AWS Backup publishes a plan's mutations under, spelled
/// out here rather than imported from the service crate, so a wrong constant in the
/// implementation cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.backup";
const BACKUP_PLAN_STATE_CHANGE: &str = "Backup Plan State Change";

/// The two transitions `detail.state` reports for a plan: the update and the delete of
/// one. The plan a CREATE stored is what that call's own response reports, so it has no
/// envelope of its own.
const MODIFIED: &str = "MODIFIED";
const DELETED: &str = "DELETED";

/// A `detail-type` of this same source that is NOT a plan's — a backup job's — so a row
/// can install a pattern that matches this service but no plan mutation.
const BACKUP_JOB_STATE_CHANGE: &str = "Backup Job State Change";

/// The vault every row's plan rules target, inside the modelled `BackupVaultName` pattern
/// `^[a-zA-Z0-9\-\_]{2,50}$`.
const VAULT: &str = "plan-events-vault";

/// How long a poll loop looks for an envelope before failing the case.
///
/// Generous on purpose: the loop waits for the ENVELOPE, looked for again until it is
/// there, and this deadline only bounds one that is never published at all — so a loaded
/// or a slow host still sees one that is.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one attempt of a poll loop waits before looking again.
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The `DeploymentStatus` of an audit framework whose deployment has settled, as the
/// model lists it for `Framework$DeploymentStatus`.
const DEPLOYMENT_COMPLETED: &str = "COMPLETED";

/// AWS Backup, EventBridge and Amazon SQS over ONE engine, as `core/router/src/lib.rs`
/// mounts them: the publication reaches EventBridge through the in-process callback, and a
/// rule on that bus delivers to the queue this suite reads.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_backup_cryocontrollerusermanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// The status and parsed body of one dispatched request (`Value::Null` for an empty or
/// unparseable body).
async fn answered(router: &MultiServiceRouter, request: http::Request<Bytes>) -> (u16, Value) {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One AWS Backup call (`restJson1`), with whatever status it answered.
async fn backup_answered(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |body| Bytes::from(body.to_string()));
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://backup.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("backup.{REGION}.amazonaws.com"))
        .header("authorization", authorization("backup"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder.body(payload).expect("a well-formed test request");
    answered(router, request).await
}

/// The same call, which must answer 200.
async fn backup(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Value {
    let (status, parsed) = backup_answered(router, method, uri, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Backup {method} {uri}, got {status} with {parsed}"
    );
    parsed
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`), which must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header("authorization", authorization("events"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed test request");
    let (status, parsed) = answered(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`), which must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed test request");
    let (status, parsed) = answered(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A queue called `name`, carrying the grant a rule's target needs — EventBridge makes the
/// target's `sqs:SendMessage` as its own service principal and relies on the queue's
/// resource policy for it, so a queue that has granted it nothing receives nothing.
/// Answers the queue's url and the ARN a rule names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// Install an ENABLED rule called `name` whose `EventPattern` is `pattern` on the default
/// bus, with the queue `target_arn` as its only target.
async fn given_rule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    pattern: &Value,
    target_arn: &str,
) {
    events(
        router,
        case,
        "PutRule",
        &json!({ "Name": name, "State": "ENABLED", "EventPattern": pattern.to_string() }),
    )
    .await;
    events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "Targets": [{ "Id": format!("{name}-target"), "Arn": target_arn }],
        }),
    )
    .await;
}

/// Every envelope waiting on the queue at `url`, oldest first.
///
/// Drained in several receives because one `ReceiveMessage` answers at most ten messages
/// and a received message is invisible afterwards, so the loop sees each message once. It
/// stops on the first empty receive, and after at most `MAX_RECEIVES` of them whatever the
/// queue holds.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    /// How many `ReceiveMessage` calls one drain makes at most.
    const MAX_RECEIVES: usize = 4;
    let mut envelopes = Vec::new();
    for _ in 0..MAX_RECEIVES {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            break;
        }
        for message in messages {
            let raw = message["Body"].as_str().unwrap_or_default();
            envelopes.push(serde_json::from_str::<Value>(raw).unwrap_or(Value::Null));
        }
    }
    envelopes
}

/// Poll the queue at `url` until it has handed over at least `wanted` envelopes, or until
/// [`POLL_DEADLINE`] passes.
///
/// The case waits for the ENVELOPES rather than for a span: one not delivered yet is
/// looked for again, and one that is never delivered fails the case at the deadline rather
/// than hanging it. A drain takes what it reports off the queue, so what each attempt
/// collects is kept.
async fn delivered_at_least(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut envelopes = Vec::new();
    loop {
        envelopes.extend(drain(router, case, url).await);
        if envelopes.len() >= wanted {
            return envelopes;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {wanted} envelope(s) on {url:?} within \
             {POLL_DEADLINE:?}, got {envelopes:?}"
        );
        // Hand the runtime back, so anything the emulator is doing beside this task makes
        // progress between attempts rather than only when this one happens to block.
        ::tokio::task::yield_now().await;
    }
}

/// The framework `name` once its deployment has settled, looked for again until it has or
/// until [`POLL_DEADLINE`] passes: that transition falls due on the emulator's own clock,
/// so the case waits for the STATUS rather than for a span. A framework still deploying
/// refuses the update that follows, which is a concurrent-update refusal rather than
/// anything about the bus.
async fn settled_framework(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = backup(
            router,
            case,
            "GET",
            &format!("/audit/frameworks/{name}"),
            None,
        )
        .await;
        if described["DeploymentStatus"] == json!(DEPLOYMENT_COMPLETED) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the framework's deployment to settle within \
             {POLL_DEADLINE:?}, got {described}"
        );
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// A string member of a JSON body, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// One rule of a plan body: the two `@required` members and the windows and schedule a
/// client configures beside them.
fn plan_rule(name: &str, schedule: &str, start: i64, completion: i64) -> Value {
    json!({
        "RuleName": name,
        "TargetBackupVaultName": VAULT,
        "ScheduleExpression": schedule,
        "StartWindowMinutes": start,
        "CompletionWindowMinutes": completion,
    })
}

/// What ONE mutation of the plan is expected to publish: the transition it made, and the
/// `VersionId` the mutation's own response reported for the plan.
#[derive(Debug, Clone)]
struct Expected {
    state: &'static str,
    version_id: String,
}

/// One row of the table: the pattern a client installs, which of the plan's transitions
/// that pattern is owed, the body the create stores, the bodies the updates store, and
/// whether the plan is deleted at the end.
struct Row {
    case: &'static str,
    pattern: Value,
    /// Whether the row's own rule is owed the envelope of a transition into `state`.
    selects: fn(&str) -> bool,
    /// The `BackupPlan` the create is given, without its name.
    created: Value,
    /// The `BackupPlan` each update is given, without its name — one per update.
    updates: Vec<Value>,
    deleted: bool,
}

/// Every transition is owed to a pattern that filters on the source alone, or on the
/// source together with the plan's own detail-type.
fn every_state(_state: &str) -> bool {
    true
}

/// No transition of a plan is owed to a pattern naming another record's detail-type.
fn no_state(_state: &str) -> bool {
    false
}

/// Only the update's transition is owed to a pattern that filters on that one
/// `detail.state`.
fn modified_only(state: &str) -> bool {
    state == MODIFIED
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a source-only pattern, one rule, one update, then deleted",
            pattern: json!({ "source": [EVENT_SOURCE] }),
            selects: every_state,
            created: json!({ "Rules": [plan_rule("daily", "cron(0 5 * * ? *)", 60, 120)] }),
            updates: vec![json!({
                "Rules": [plan_rule("daily", "cron(0 6 * * ? *)", 90, 180)],
            })],
            deleted: true,
        },
        Row {
            case: "a pattern naming the plan detail-type, two rules, two updates, kept",
            pattern: json!({
                "source": [EVENT_SOURCE],
                "detail-type": [BACKUP_PLAN_STATE_CHANGE],
            }),
            selects: every_state,
            created: json!({
                "Rules": [
                    plan_rule("daily", "cron(0 5 * * ? *)", 60, 120),
                    plan_rule("weekly", "cron(0 7 ? * SAT *)", 120, 360),
                ],
            }),
            updates: vec![
                json!({
                    "Rules": [
                        plan_rule("daily", "cron(30 5 * * ? *)", 60, 120),
                        plan_rule("weekly", "cron(0 8 ? * SUN *)", 120, 360),
                    ],
                }),
                json!({
                    "Rules": [plan_rule("daily", "cron(0 4 * * ? *)", 180, 240)],
                }),
            ],
            deleted: false,
        },
        Row {
            case: "a pattern naming a LIST of detail-types, advanced backup settings, no update",
            pattern: json!({
                "source": [EVENT_SOURCE],
                "detail-type": [BACKUP_JOB_STATE_CHANGE, BACKUP_PLAN_STATE_CHANGE],
            }),
            selects: every_state,
            created: json!({
                "Rules": [plan_rule("daily", "cron(0 5 * * ? *)", 60, 120)],
                "AdvancedBackupSettings": [{
                    "ResourceType": "EC2",
                    "BackupOptions": { "WindowsVSS": "enabled" },
                }],
            }),
            updates: Vec::new(),
            deleted: true,
        },
        Row {
            case: "a pattern naming another record's detail-type only",
            pattern: json!({
                "source": [EVENT_SOURCE],
                "detail-type": [BACKUP_JOB_STATE_CHANGE],
            }),
            selects: no_state,
            created: json!({ "Rules": [plan_rule("daily", "cron(0 5 * * ? *)", 60, 120)] }),
            updates: vec![json!({
                "Rules": [plan_rule("daily", "cron(0 9 * * ? *)", 60, 120)],
            })],
            deleted: true,
        },
        Row {
            case: "a pattern filtering on one detail.state",
            pattern: json!({
                "source": [EVENT_SOURCE],
                "detail-type": [BACKUP_PLAN_STATE_CHANGE],
                "detail": { "state": [MODIFIED] },
            }),
            selects: modified_only,
            created: json!({ "Rules": [plan_rule("daily", "cron(0 5 * * ? *)", 60, 120)] }),
            updates: vec![json!({
                "Rules": [plan_rule("daily", "cron(15 5 * * ? *)", 75, 150)],
            })],
            deleted: true,
        },
    ]
}

/// A plan body with `name` as its `BackupPlanName` and `body`'s members beside it.
fn named(name: &str, body: &Value) -> Value {
    let mut plan = body.as_object().cloned().unwrap_or_default();
    plan.insert("BackupPlanName".to_string(), json!(name));
    Value::Object(plan)
}

/// Assert that `envelope` is the plan state change `expected` describes, for the plan
/// `plan_id` whose ARN is `plan_arn` and whose name is `plan_name`.
fn assert_envelope(
    case: &str,
    envelope: &Value,
    expected: &Expected,
    plan_id: &str,
    plan_arn: &str,
    plan_name: &str,
) {
    assert_eq!(
        (
            envelope["source"].clone(),
            envelope["detail-type"].clone(),
            envelope["resources"].clone(),
        ),
        (
            json!(EVENT_SOURCE),
            json!(BACKUP_PLAN_STATE_CHANGE),
            json!([plan_arn]),
        ),
        "case {case:?}: expected the {} envelope of this plan, got {envelope}",
        expected.state
    );
    assert_eq!(
        (
            envelope["detail"]["backupPlanId"].clone(),
            envelope["detail"]["state"].clone(),
            envelope["detail"]["versionId"].clone(),
            envelope["detail"]["backupPlanArn"].clone(),
            envelope["detail"]["backupPlanName"].clone(),
        ),
        (
            json!(plan_id),
            json!(expected.state),
            json!(expected.version_id),
            json!(plan_arn),
            json!(plan_name),
        ),
        "case {case:?}: expected the plan, the version and the state the mutation reported, \
         got {envelope}"
    );
    assert!(
        envelope["detail"]["creationDate"].is_string(),
        "case {case:?}: the detail reports the stored creation instant, got {envelope}"
    );
    // The deletion instant is the plan's own, so it is reported once the plan carries one
    // and absent from every envelope published before it.
    let deletion_date = &envelope["detail"]["deletionDate"];
    if expected.state == DELETED {
        assert!(
            deletion_date.is_string(),
            "case {case:?}: the deletion envelope reports the stored deletion instant, got \
             {envelope}"
        );
    } else {
        assert_eq!(
            deletion_date,
            &Value::Null,
            "case {case:?}: a live plan carries no deletion instant, got {envelope}"
        );
    }
}

/// The transitions a plan's own mutations publish, in order, delivered to every rule whose
/// pattern matches them and to no rule whose pattern does not.
#[tokio::test]
async fn every_backup_plan_mutation_is_published_as_a_plan_state_change() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let router = router();
        backup(
            &router,
            case,
            "PUT",
            &format!("/backup-vaults/{VAULT}"),
            Some(&json!({})),
        )
        .await;

        // The row's own rule, and the CONTROL rule matching the source alone: the
        // control's envelopes are what say the publication happened at all.
        let (queue, queue_arn) = given_queue(&router, case, &format!("plan-events-{index}")).await;
        given_rule(
            &router,
            case,
            &format!("plan-events-{index}"),
            &row.pattern,
            &queue_arn,
        )
        .await;
        let (control, control_arn) =
            given_queue(&router, case, &format!("plan-events-control-{index}")).await;
        given_rule(
            &router,
            case,
            &format!("plan-events-control-{index}"),
            &json!({ "source": [EVENT_SOURCE] }),
            &control_arn,
        )
        .await;

        // The mutations, each of which owes ONE envelope: one per update, then the delete.
        // What each published is asserted against the `VersionId` that call's own response
        // reported. The create itself owes none, which is read right after it.
        let plan_name = format!("plan-events-{index}");
        let created = backup(
            &router,
            case,
            "PUT",
            "/backup/plans",
            Some(&json!({ "BackupPlan": named(&plan_name, &row.created) })),
        )
        .await;
        let plan_id = member(case, &created, "/BackupPlanId");
        let plan_arn = member(case, &created, "/BackupPlanArn");
        // The plan a create stored is what the create's own response reports, so the bus
        // carries nothing for it. The publication a mutation owes is made by the mutating
        // call itself, so a drain taken once that call has answered is the whole of what
        // it published.
        let after_create = drain(&router, case, &control).await;
        assert!(
            after_create.is_empty(),
            "case {case:?}: a create publishes no plan state change, got {after_create:?}"
        );
        let mut expected: Vec<Expected> = Vec::new();
        for update in &row.updates {
            let updated = backup(
                &router,
                case,
                "POST",
                &format!("/backup/plans/{plan_id}"),
                Some(&json!({ "BackupPlan": named(&plan_name, update) })),
            )
            .await;
            assert_eq!(
                (
                    updated["BackupPlanId"].clone(),
                    updated["BackupPlanArn"].clone()
                ),
                (json!(&plan_id), json!(&plan_arn)),
                "case {case:?}: an update keeps the plan's identifiers, got {updated}"
            );
            expected.push(Expected {
                state: MODIFIED,
                version_id: member(case, &updated, "/VersionId"),
            });
        }
        if row.deleted {
            let deleted = backup(
                &router,
                case,
                "DELETE",
                &format!("/backup/plans/{plan_id}"),
                None,
            )
            .await;
            expected.push(Expected {
                state: DELETED,
                version_id: member(case, &deleted, "/VersionId"),
            });
        }

        // The control rule is owed every one of them, in the order the mutations were
        // made.
        let seen = delivered_at_least(&router, case, &control, expected.len()).await;
        assert_eq!(
            seen.len(),
            expected.len(),
            "case {case:?}: the plan's mutations are the ONLY envelopes the bus carries, got \
             {seen:?}"
        );
        for (envelope, owed) in seen.iter().zip(expected.iter()) {
            assert_envelope(case, envelope, owed, &plan_id, &plan_arn, &plan_name);
        }

        // And the row's own rule is owed exactly the transitions its pattern selects —
        // read AFTER the control's, so a row owed nothing is a refusal to deliver rather
        // than a look taken too early.
        let owed: Vec<Expected> = expected
            .iter()
            .filter(|expected| (row.selects)(expected.state))
            .cloned()
            .collect();
        let selected = if owed.is_empty() {
            drain(&router, case, &queue).await
        } else {
            delivered_at_least(&router, case, &queue, owed.len()).await
        };
        assert_eq!(
            selected.len(),
            owed.len(),
            "case {case:?}: expected the {} envelope(s) this pattern selects, got {selected:?}",
            owed.len()
        );
        for (envelope, owed) in selected.iter().zip(owed.iter()) {
            assert_envelope(case, envelope, owed, &plan_id, &plan_arn, &plan_name);
        }
    }
}

/// An audit framework has no detail-type of this source: its create and its update publish
/// nothing, on a bus a plan's own mutation reaches.
///
/// The negative half is only worth anything beside that positive one: a bus that published
/// nothing ever would satisfy "no framework envelope" vacuously, so the framework calls are
/// made BETWEEN two plan mutations whose envelopes ARE delivered to the same rule and the
/// same target — anything the framework published would be queued between them.
#[tokio::test]
async fn a_frameworks_create_and_update_publish_no_event() {
    let case = "a framework's mutations on a bus a plan's reach";
    let router = router();
    let (queue, queue_arn) = given_queue(&router, case, "framework-events").await;
    // Owed EVERY envelope of this source: a rule filtering any of them out could not tell
    // "withheld" from "filtered".
    given_rule(
        &router,
        case,
        "framework-events",
        &json!({ "source": [EVENT_SOURCE] }),
        &queue_arn,
    )
    .await;
    backup(
        &router,
        case,
        "PUT",
        &format!("/backup-vaults/{VAULT}"),
        Some(&json!({})),
    )
    .await;

    // The first plan mutation, and its envelope: the path is live before any framework
    // call. The create that precedes it publishes nothing, so the update is what the queue
    // is first handed.
    let created = backup(
        &router,
        case,
        "PUT",
        "/backup/plans",
        Some(&json!({
            "BackupPlan": named(
                "framework-events-plan",
                &json!({ "Rules": [plan_rule("daily", "cron(0 5 * * ? *)", 60, 120)] }),
            ),
        })),
    )
    .await;
    let plan_id = member(case, &created, "/BackupPlanId");
    let plan_arn = member(case, &created, "/BackupPlanArn");
    let updated = backup(
        &router,
        case,
        "POST",
        &format!("/backup/plans/{plan_id}"),
        Some(&json!({
            "BackupPlan": named(
                "framework-events-plan",
                &json!({ "Rules": [plan_rule("daily", "cron(0 6 * * ? *)", 90, 180)] }),
            ),
        })),
    )
    .await;
    let mut seen = delivered_at_least(&router, case, &queue, 1).await;
    assert_eq!(
        seen.len(),
        1,
        "case {case:?}: the update owes exactly one envelope, got {seen:?}"
    );
    assert_envelope(
        case,
        &seen[0],
        &Expected {
            state: MODIFIED,
            version_id: member(case, &updated, "/VersionId"),
        },
        &plan_id,
        &plan_arn,
        "framework-events-plan",
    );

    // The framework's own create and update, which owe nothing: `POST /audit/frameworks`
    // and `PUT /audit/frameworks/{FrameworkName}`, as the model binds them.
    let framework = "PlanEventsFramework";
    backup(
        &router,
        case,
        "POST",
        "/audit/frameworks",
        Some(&json!({
            "FrameworkName": framework,
            "FrameworkDescription": "as created",
            "FrameworkControls": [{ "ControlName": "BACKUP_RECOVERY_POINT_ENCRYPTED" }],
        })),
    )
    .await;
    // The framework is returned in a non-terminal deployment status, and an update of one
    // still deploying is refused: the case waits for that status before updating.
    settled_framework(&router, case, framework).await;
    backup(
        &router,
        case,
        "PUT",
        &format!("/audit/frameworks/{framework}"),
        Some(&json!({
            "FrameworkDescription": "as updated",
            "FrameworkControls": [
                { "ControlName": "BACKUP_RECOVERY_POINT_MINIMUM_RETENTION_CHECK",
                  "ControlInputParameters": [
                      { "ParameterName": "requiredRetentionDays", "ParameterValue": "35" },
                  ] },
            ],
        })),
    )
    .await;

    // The second plan mutation: its envelope is the NEXT thing the queue hands over, which
    // is only true if the framework calls in between published nothing.
    let deleted = backup(
        &router,
        case,
        "DELETE",
        &format!("/backup/plans/{plan_id}"),
        None,
    )
    .await;
    seen.extend(delivered_at_least(&router, case, &queue, 1).await);
    assert_eq!(
        seen.len(),
        2,
        "case {case:?}: the two plan mutations are the only envelopes the bus carries, got \
         {seen:?}"
    );
    assert_envelope(
        case,
        &seen[1],
        &Expected {
            state: DELETED,
            version_id: member(case, &deleted, "/VersionId"),
        },
        &plan_id,
        &plan_arn,
        "framework-events-plan",
    );

    // Nothing else lands afterwards either: the drain that follows is empty, so no
    // framework call's publication arrived late.
    let trailing = drain(&router, case, &queue).await;
    assert!(
        trailing.is_empty(),
        "case {case:?}: no further envelope is owed, got {trailing:?}"
    );
}

/// A mutation this service REFUSES publishes nothing: there is no plan transition to
/// announce, so an update and a delete of an id this account holds no plan under leave the
/// bus carrying only the envelope of the mutation it ACCEPTED.
#[tokio::test]
async fn a_refused_plan_mutation_publishes_nothing() {
    let case = "a refused update and a refused delete";
    let router = router();
    let (queue, queue_arn) = given_queue(&router, case, "refused-plan-events").await;
    given_rule(
        &router,
        case,
        "refused-plan-events",
        &json!({ "source": [EVENT_SOURCE] }),
        &queue_arn,
    )
    .await;
    // The vault this row's rules target: an update resolves every rule's
    // `TargetBackupVaultName` in the caller's Region before it stores a version, so the
    // account holds it before the accepted mutations below.
    backup(
        &router,
        case,
        "PUT",
        &format!("/backup-vaults/{VAULT}"),
        Some(&json!({})),
    )
    .await;

    // An id of the form this service mints that names no plan: the update and the delete of
    // it are refused, and the model gives each refusal HTTP 400.
    let absent = "00000000-0000-0000-0000-000000000000";
    for (method, body) in [
        (
            "POST",
            Some(json!({
                "BackupPlan": named(
                    "never-stored",
                    &json!({ "Rules": [plan_rule("daily", "cron(0 5 * * ? *)", 60, 120)] }),
                ),
            })),
        ),
        ("DELETE", None),
    ] {
        let (status, refused) = backup_answered(
            &router,
            method,
            &format!("/backup/plans/{absent}"),
            body.as_ref(),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from Backup {method} of an absent plan, got \
             {status} with {refused}"
        );
    }

    // The plan the accepted mutations below are made on, whose own create publishes
    // nothing.
    let created = backup(
        &router,
        case,
        "PUT",
        "/backup/plans",
        Some(&json!({
            "BackupPlan": named(
                "refused-events-plan",
                &json!({ "Rules": [plan_rule("daily", "cron(0 5 * * ? *)", 60, 120)] }),
            ),
        })),
    )
    .await;
    let plan_id = member(case, &created, "/BackupPlanId");
    let plan_arn = member(case, &created, "/BackupPlanArn");

    // The accepted update that follows is the first thing the queue is handed, which is
    // only true if the refusals published nothing.
    let updated = backup(
        &router,
        case,
        "POST",
        &format!("/backup/plans/{plan_id}"),
        Some(&json!({
            "BackupPlan": named(
                "refused-events-plan",
                &json!({ "Rules": [plan_rule("daily", "cron(0 7 * * ? *)", 60, 120)] }),
            ),
        })),
    )
    .await;
    let seen = delivered_at_least(&router, case, &queue, 1).await;
    assert_eq!(
        seen.len(),
        1,
        "case {case:?}: the accepted update's envelope is the ONE the bus carries, got \
         {seen:?}"
    );
    assert_envelope(
        case,
        &seen[0],
        &Expected {
            state: MODIFIED,
            version_id: member(case, &updated, "/VersionId"),
        },
        &plan_id,
        &plan_arn,
        "refused-events-plan",
    );
}
