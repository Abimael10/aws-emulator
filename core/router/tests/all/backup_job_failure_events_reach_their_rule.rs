// SPDX-License-Identifier: CC0-1.0

//! A backup job's TERMINAL FAILED transition is published on the account's default
//! EventBridge bus exactly as its successful one is, and a rule whose pattern scopes
//! `detail.state` receives the transitions whose state it names and no other.
//!
//! The RULE pinned here: **AWS Backup publishes every state a backup job MOVES INTO onto the
//! account's default EventBridge bus of the Region the job's VAULT belongs to, as `source`
//! `aws.backup` / `detail-type` `Backup Job State Change`, carrying the job's own stored
//! members as the event `detail` — and that holds for a TERMINAL state the job reaches because
//! the backup could not be performed at all (the role it runs under may not create the
//! recovery point, the resource it was started for is held by nobody) exactly as it holds for
//! the one it reaches when the backup succeeds. EventBridge then routes each envelope by its
//! CONTENT, so a rule whose pattern scopes `detail.state` receives the transitions into the
//! states it lists and none of the job's other transitions.**
//!
//! Source for the envelope: the AWS Backup EventBridge notifications, which carry
//! `"source": "aws.backup"` with the `detail-type` of the kind of record that moved; the
//! backup Smithy model describes the control plane and not the bus, so each `detail` member
//! is asserted against what the SAME job's control-plane read reports for it, and the states
//! a job may be in against `smithy_query(query="shape",
//! shape="com.amazonaws.backup#BackupJobState")`, whose terminal failure value is `FAILED`.
//! The source, the detail-type and the states are spelled out here rather than imported from
//! the service crate, so a wrong constant in the implementation cannot make this suite pass.
//!
//! Five crates have to agree for that to hold — Amazon DynamoDB holds the protected table,
//! IAM holds the role the backup is performed under and the documents that decide what it may
//! do, AWS Backup runs the job and publishes its moves, EventBridge matches each envelope
//! against each rule's pattern, and Amazon SQS holds what arrived under its own queue policy
//! — so the suite lives here rather than in any one crate, and it runs over the WHOLE
//! `build_router` service set the binary serves.
//!
//! The rows differ along exactly what the rule generalises over: WHY the job reaches its
//! terminal state (a role with no permissions at all, a role granted everything but a
//! resource nobody holds, a role granted everything over a resource that is held) and so
//! along which terminal state it reaches; and the whole table is run once per REGION, since
//! the bus an envelope reaches is the one of the Region the job's vault belongs to while IAM,
//! which answers for the role, is global. Every row publishes against the SAME two rules of
//! its Region: one scoped to `detail.state: [FAILED]` with an Amazon SQS target whose queue
//! policy admits this service's writes only for that rule's own ARN, and one scoped to the
//! source alone as the POSITIVE CONTROL — so a row that must deliver nothing to the scoped
//! queue is read against a queue that did receive the job's transitions rather than against a
//! bare quiet window.
//!
//! What is NOT asserted here, because it is asserted elsewhere:
//! `crate::backup_job_events_are_scoped_by_detail_type` pins the `detail-type` scoping and the
//! successful job's envelope members,
//! `crate::backup_failed_job_event_on_a_started_emulator` pins this same FAILED delivery on the
//! emulator a client STARTS (signed calls, authentication on, the binary's own runtime),
//! `services/service-com-amazonaws-backup-cryocontrollerusermanager/tests/all/job_state_change_events.rs`
//! pins the published `detail` of every job kind against the control plane's own reads, and
//! `crate::backup_job_state_change_invokes_its_lambda_target` pins the same publication routed
//! to a Lambda target. What only this suite reaches is the FAILED half of the contract: a job
//! whose backup cannot be performed, its terminal transition on the bus, and a rule that
//! admits that state alone reading it off its own queue.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery IAM caller and the XML reader this test crate writes ONCE rather than per
// suite. IAM is a GLOBAL service, reached at one endpoint whatever Region the caller works
// in, which is why the shared caller serves every Region's row here.
use crate::query_wire::element;
use crate::sns_lambda_subscription_permission_boundary::iam;

/// The `source` AWS Backup publishes under and the `detail-type` a backup job's own
/// transitions are published with.
const BACKUP_SOURCE: &str = "aws.backup";
const BACKUP_JOB_STATE_CHANGE: &str = "Backup Job State Change";

/// The service principal EventBridge delivers to a queue as, and the write it makes.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The service principal a backup role trusts, as the role a backup is performed under is
/// assumed by AWS Backup itself.
const BACKUP_PRINCIPAL: &str = "backup.amazonaws.com";

/// The state a backup job is stored in by the call that starts it, and the two terminal
/// states a job reaches — the moves the bus is owed.
const CREATED: &str = "CREATED";
const COMPLETED: &str = "COMPLETED";
const FAILED: &str = "FAILED";

/// Every state a backup job may be IN, as the model's `BackupJobState` declares them
/// (`smithy_query(query="shape", shape="com.amazonaws.backup#BackupJobState")`), so a
/// published state is checked against the catalog rather than against the one a row's job
/// happens to take.
const JOB_STATES: [&str; 9] = [
    "CREATED",
    "PENDING",
    "RUNNING",
    "ABORTING",
    "ABORTED",
    "COMPLETED",
    "FAILED",
    "EXPIRED",
    "PARTIAL",
];

/// How long a row waits for the deliveries it must see, how long it keeps looking for one it
/// must never see, and how often either looks. Each is generous: nothing here is a
/// measurement of how fast the host runs.
const DELIVERY_BUDGET: Duration = Duration::from_secs(60);
const QUIET_WINDOW: Duration = Duration::from_secs(3);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as `core/router/src/lib.rs` assembles it for
/// the binary: the publication AWS Backup makes is dispatched back into this same router, so
/// the rules this suite installs are the rules that publication is matched against.
fn router() -> MultiServiceRouter {
    let mut router = aws_emulator_router::build_router(aws_emulator_router::new_engine());
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` in `region`, which is what
/// routes the request and what the Region of everything it mints is read off. The signature is
/// never checked: the router above runs with request authentication off.
fn authorization(region: &str, service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The status and parsed body of one response (`Value::Null` for an empty or unparseable
/// body).
fn answered(response: &http::Response<Bytes>) -> (u16, Value) {
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One `X-Amz-Target` RPC call against `signing_name` in `region`, which must answer 200.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    signing_name: &str,
    target: &str,
    content_type: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("authorization", authorization(region, signing_name))
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(&router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target} in {region}, got {status} with {parsed}"
    );
    parsed
}

/// One EventBridge call (`awsJson1_1`), which must answer 200.
async fn events(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        case,
        region,
        "events",
        &format!("AWSEvents.{operation}"),
        "application/x-amz-json-1.1",
        body,
    )
    .await
}

/// One Amazon SQS call (`awsJson1_0`), which must answer 200.
async fn sqs(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        case,
        region,
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
    .await
}

/// One Amazon DynamoDB call (`awsJson1_0`), which must answer 200.
async fn dynamodb(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        case,
        region,
        "dynamodb",
        &format!("DynamoDB_20120810.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
    .await
}

/// One AWS Backup call (`restJson1`), which must answer 200.
async fn backup(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Value {
    let payload = body.map_or_else(Bytes::new, |body| Bytes::from(body.to_string()));
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://backup.{region}.amazonaws.com{uri}"))
        .header("host", format!("backup.{region}.amazonaws.com"))
        .header("authorization", authorization(region, "backup"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(payload)
        .expect("the test request is well formed");
    let (status, parsed) = answered(&router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Backup {method} {uri} in {region}, got {status} \
         with {parsed}"
    );
    parsed
}

/// A string member of a response, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// A queue of `region` whose own policy admits the write EventBridge makes to it ("for …
/// Amazon SQS queues, EventBridge relies on resource-based policies", `PutTargets`), scoped —
/// when `rule_arn` names one — to the deliveries of THAT rule through `aws:SourceArn`, which
/// is how a queue is granted to one rule's targets alone. Answers its url and the ARN a target
/// names it by, read back from the queue itself.
async fn granted_queue(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    rule_arn: Option<&str>,
) -> (String, String) {
    let created = sqs(router, case, region, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = member(case, &created, "/QueueUrl");
    let attributes = sqs(
        router,
        case,
        region,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = member(case, &attributes, "/Attributes/QueueArn");
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "Service": EVENTS_PRINCIPAL },
        "Action": SEND_MESSAGE_ACTION,
        "Resource": &arn,
    });
    if let Some(rule_arn) = rule_arn {
        statement["Condition"] = json!({ "ArnEquals": { "aws:SourceArn": rule_arn } });
    }
    let policy = json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string();
    sqs(
        router,
        case,
        region,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// The trust document of a role AWS Backup may act as.
fn trusting_backup() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": BACKUP_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create the role `name` where IAM holds it — under `path` when the row names one, as a role
/// a client provisions for a service routinely is — trusted by AWS Backup and carrying an
/// inline policy allowing `allows` when the row names one, and answer the ARN IAM ITSELF
/// reports for it.
///
/// `allows` `None` is a role with NO permissions policy at all, and a row naming an action
/// OTHER than the one a backup performs is a role whose policy set says nothing about that
/// backup: under either, no backup can be performed.
async fn backup_role(
    router: &MultiServiceRouter,
    name: &str,
    path: Option<&str>,
    allows: Option<&str>,
) -> String {
    let trust = trusting_backup();
    let mut members: Vec<(&str, &str)> = vec![
        ("RoleName", name),
        ("AssumeRolePolicyDocument", trust.as_str()),
    ];
    if let Some(path) = path {
        members.push(("Path", path));
    }
    let created = iam(router, "CreateRole", &members).await;
    if let Some(allows) = allows {
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": allows, "Resource": "*" }],
        })
        .to_string();
        iam(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "stated"),
                ("PolicyDocument", &policy),
            ],
        )
        .await;
    }
    element(&created, "Arn")
}

/// One row: what the job is started with, and the terminal state it must therefore reach and
/// publish.
struct Row {
    case: &'static str,
    /// The IAM path the role is held under, `None` for a role at the account root.
    role_path: Option<&'static str>,
    /// The one action the role's inline policy allows, `None` for a role carrying NO
    /// permissions policy at all.
    role_allows: Option<&'static str>,
    /// Whether the resource the job is started for is one the service that owns it holds.
    resource_held: bool,
    /// The terminal state the job must reach, and so the state its last published envelope
    /// must carry.
    terminal: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a role that trusts the service but carries no permissions policy",
        role_path: None,
        role_allows: None,
        resource_held: true,
        terminal: FAILED,
    },
    Row {
        case: "a permission-less role held under a path",
        role_path: Some("/service-role/"),
        role_allows: None,
        resource_held: true,
        terminal: FAILED,
    },
    Row {
        case: "a role whose policy allows another service's action only",
        role_path: None,
        role_allows: Some("s3:GetObject"),
        resource_held: true,
        terminal: FAILED,
    },
    Row {
        case: "a granted role over a resource nobody holds",
        role_path: None,
        role_allows: Some("*"),
        resource_held: false,
        terminal: FAILED,
    },
    Row {
        case: "a granted role over a resource that is held",
        role_path: None,
        role_allows: Some("*"),
        resource_held: true,
        terminal: COMPLETED,
    },
];

/// Every envelope the queue at `url` has received, parsed, accumulating across looks.
async fn received(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    seen: &mut Vec<Value>,
    url: &str,
) {
    let answered = sqs(
        router,
        case,
        region,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    for message in answered["Messages"].as_array().cloned().unwrap_or_default() {
        if let Some(Ok(envelope)) = message["Body"]
            .as_str()
            .map(serde_json::from_str::<Value>)
        {
            seen.push(envelope);
        }
    }
}

/// The job envelopes among `seen` that name `job_id` in their detail.
fn job_envelopes<'a>(seen: &'a [Value], job_id: &str) -> Vec<&'a Value> {
    seen.iter()
        .filter(|envelope| {
            envelope["detail-type"] == json!(BACKUP_JOB_STATE_CHANGE)
                && envelope["detail"]["backupJobId"] == json!(job_id)
        })
        .collect()
}

/// RULE: a backup job's terminal transition — the FAILED one of a backup that cannot be
/// performed included — is published as an `aws.backup` job-state-change envelope on the bus
/// of the job's own Region, and a rule scoped to `detail.state` receives exactly the
/// transitions into the state it names.
async fn every_terminal_state_reaches_the_rule_that_names_it(region: &str) {
    let router = router();
    let setup = "the rules and the vault every row publishes against";

    // The rule scoped to the terminal FAILED state, and the control rule that admits every
    // envelope this source publishes — both installed BEFORE anything is published, since a
    // rule installed after a publication has nothing to be handed.
    let failed_rule = format!("backup-failed-state-rule-{region}");
    let failed_pattern = json!({
        "source": [BACKUP_SOURCE],
        "detail-type": [BACKUP_JOB_STATE_CHANGE],
        "detail": { "state": [FAILED] },
    });
    let put = events(
        &router,
        setup,
        region,
        "PutRule",
        &json!({
            "Name": &failed_rule,
            "EventPattern": failed_pattern.to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let failed_rule_arn = member(setup, &put, "/RuleArn");
    let (failed_url, failed_queue_arn) = granted_queue(
        &router,
        setup,
        region,
        &format!("backup-failed-state-queue-{region}"),
        Some(&failed_rule_arn),
    )
    .await;
    let wired = events(
        &router,
        setup,
        region,
        "PutTargets",
        &json!({
            "Rule": &failed_rule,
            "Targets": [{ "Id": "sink", "Arn": &failed_queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {setup:?}: the queue target must be accepted, got {wired}"
    );

    let control_rule = format!("backup-every-state-rule-{region}");
    events(
        &router,
        setup,
        region,
        "PutRule",
        &json!({
            "Name": &control_rule,
            "EventPattern": json!({ "source": [BACKUP_SOURCE] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let (control_url, control_queue_arn) = granted_queue(
        &router,
        setup,
        region,
        &format!("backup-every-state-queue-{region}"),
        None,
    )
    .await;
    let wired = events(
        &router,
        setup,
        region,
        "PutTargets",
        &json!({
            "Rule": &control_rule,
            "Targets": [{ "Id": "sink", "Arn": &control_queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {setup:?}: the control rule's queue target must be accepted, got {wired}"
    );

    // The table the held rows protect, and the vault every row's job writes into.
    let table = dynamodb(
        &router,
        setup,
        region,
        "CreateTable",
        &json!({
            "TableName": "backup-failure-table",
            "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let held_arn = member(setup, &table, "/TableDescription/TableArn");
    // A table of the same account and Region that DynamoDB holds nothing under: a backup
    // started for it has nothing to capture.
    let absent_arn = format!("{held_arn}-absent");
    let vault = "backup-failure-vault";
    backup(
        &router,
        setup,
        region,
        "PUT",
        &format!("/backup-vaults/{vault}"),
        Some(&json!({})),
    )
    .await;

    let mut scoped_seen: Vec<Value> = Vec::new();
    let mut control_seen: Vec<Value> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let role = backup_role(
            &router,
            &format!("backup-failure-role-{region}-{index}"),
            row.role_path,
            row.role_allows,
        )
        .await;
        let resource_arn = if row.resource_held {
            held_arn.clone()
        } else {
            absent_arn.clone()
        };
        let started = backup(
            &router,
            case,
            region,
            "PUT",
            "/backup-jobs",
            Some(&json!({
                "BackupVaultName": vault,
                "ResourceArn": &resource_arn,
                "IamRoleArn": &role,
                "IdempotencyToken": format!("backup-failure-token-{index}"),
            })),
        )
        .await;
        let job_id = member(case, &started, "/BackupJobId");

        // The job's own reader reaches the terminal state the row expects, and a FAILED one
        // carries why it failed: a terminal state with no reason is not an outcome a client
        // can act on.
        let deadline = Instant::now() + DELIVERY_BUDGET;
        let read = loop {
            let read = backup(
                &router,
                case,
                region,
                "GET",
                &format!("/backup-jobs/{job_id}"),
                None,
            )
            .await;
            let state = member(case, &read, "/State");
            if state == row.terminal {
                break read;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected DescribeBackupJob to reach {:?} in {region}, got \
                 {state:?} in {read}",
                row.terminal
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        };
        if row.terminal == FAILED {
            let status_message = member(case, &read, "/StatusMessage");
            assert!(
                !status_message.trim().is_empty(),
                "case {case:?}: a failed job reports why it failed, got {status_message:?} in \
                 {read}"
            );
        }

        // The control rule receives the job's transitions whatever they are: that is what
        // says the publication happened at all, so a scoped queue still empty beside it is
        // the pattern's refusal rather than a look taken too early.
        let deadline = Instant::now() + DELIVERY_BUDGET;
        loop {
            received(&router, case, region, &mut control_seen, &control_url).await;
            received(&router, case, region, &mut scoped_seen, &failed_url).await;
            let terminal_published = job_envelopes(&control_seen, &job_id)
                .iter()
                .any(|envelope| envelope["detail"]["state"] == json!(row.terminal));
            if terminal_published {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the job's move into {:?} must be published in {region}, got \
                 {control_seen:?} for job {job_id:?}",
                row.terminal
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }

        // And the rule scoped to FAILED receives that transition only where the job really
        // moved into it.
        let deadline = Instant::now() + DELIVERY_BUDGET;
        loop {
            received(&router, case, region, &mut scoped_seen, &failed_url).await;
            let delivered = !job_envelopes(&scoped_seen, &job_id).is_empty();
            if row.terminal == FAILED {
                if delivered {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: the rule scoped to {FAILED:?} must receive the job's \
                     failed transition in {region}, got {scoped_seen:?} for job {job_id:?}"
                );
                tokio::time::sleep(LOOK_INTERVAL).await;
                continue;
            }
            assert!(
                !delivered,
                "case {case:?}: a job that reached {:?} owes the {FAILED:?}-scoped rule \
                 nothing, got {scoped_seen:?} for job {job_id:?}",
                row.terminal
            );
            break;
        }

        // Every envelope the scoped rule received for this job is one whose state its
        // pattern names, published under this source and kind in this Region, and carrying
        // the members the same job's control-plane read reports.
        for envelope in job_envelopes(&scoped_seen, &job_id) {
            for (pointer, expect) in [
                ("/source", json!(BACKUP_SOURCE)),
                ("/detail-type", json!(BACKUP_JOB_STATE_CHANGE)),
                ("/region", json!(region)),
                ("/detail/backupJobId", json!(&job_id)),
                ("/detail/state", json!(FAILED)),
                ("/detail/backupVaultName", json!(vault)),
                ("/detail/resourceArn", json!(&resource_arn)),
                ("/detail/iamRoleArn", json!(&role)),
                (
                    "/detail/statusMessage",
                    json!(member(case, &read, "/StatusMessage")),
                ),
                (
                    "/detail/messageCategory",
                    json!(member(case, &read, "/MessageCategory")),
                ),
            ] {
                let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    actual, expect,
                    "case {case:?}: expected {pointer} to be {expect}, got {actual} in \
                     {envelope}"
                );
            }
        }

        // The states the control rule saw for this job are states the model declares, each
        // published once, and never the state the START call itself reported.
        let mut states: Vec<String> = job_envelopes(&control_seen, &job_id)
            .iter()
            .map(|envelope| {
                envelope["detail"]["state"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        states.sort();
        assert!(
            states
                .iter()
                .all(|state| JOB_STATES.contains(&state.as_str())),
            "case {case:?}: every published state is one the model declares, got {states:?}"
        );
        assert!(
            states.windows(2).all(|pair| pair[0] != pair[1]),
            "case {case:?}: a state is published once, so none may reach the queue twice, got \
             {states:?}"
        );
        assert!(
            !states.iter().any(|state| state == CREATED),
            "case {case:?}: the state the START call stored is not a move, so it owes the bus \
             nothing, got {states:?}"
        );
    }

    // Nothing of the succeeding job reaches the FAILED-scoped queue later either, over a
    // window longer than the span between two dispatches of one publication.
    let succeeded: Vec<&Row> = ROWS.iter().filter(|row| row.terminal != FAILED).collect();
    assert!(
        !succeeded.is_empty(),
        "case {setup:?}: the suite needs a row that does NOT fail, as the negative control"
    );
    let quiet_until = Instant::now() + QUIET_WINDOW;
    while Instant::now() < quiet_until {
        received(&router, setup, region, &mut scoped_seen, &failed_url).await;
        let states: Vec<String> = scoped_seen
            .iter()
            .map(|envelope| envelope["detail"]["state"].to_string())
            .collect();
        assert!(
            scoped_seen
                .iter()
                .all(|envelope| envelope["detail"]["state"] == json!(FAILED)),
            "case {setup:?}: only the transitions into {FAILED:?} may reach this rule's queue, \
             got the states {states:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The rule, in the Region the shared callers of this test crate sign for.
#[tokio::test]
async fn failed_backup_jobs_publish_their_state_change_to_the_rule_that_names_it() {
    every_terminal_state_reaches_the_rule_that_names_it("us-east-1").await;
}

/// The SAME rule in another Region, which is what says the bus an envelope reaches is the one
/// of the Region the job's vault belongs to rather than one Region the publisher favours —
/// while the role it is judged on is held by IAM, a global service, either way.
#[tokio::test]
async fn failed_backup_jobs_publish_their_state_change_in_their_own_region() {
    every_terminal_state_reaches_the_rule_that_names_it("us-west-2").await;
}

/// How long the long-polling consumer below waits inside ONE `ReceiveMessage` call: longer
/// than the span the clock owes a started job's transition, and within the bound Amazon SQS
/// documents for `WaitTimeSeconds` ("values: 1 to 20"), so one call spans the publication.
const LONG_POLL_SECONDS: i64 = 20;

/// RULE (the delivery is owed to a consumer that is ALREADY waiting): a consumer long-polling
/// the rule's queue is handed the job's `FAILED` transition inside the call it is waiting in —
/// the publication the clock owes happens while that call waits, so a delivery made during a
/// wait is one the waiting receive answers with rather than one only a later call sees.
#[tokio::test]
async fn a_long_polling_consumer_is_handed_the_transition_published_while_it_waits() {
    let region = "us-east-1";
    let router = router();
    let case = "a consumer waiting inside one long poll";

    let rule = "backup-failed-state-long-poll-rule";
    let pattern = json!({
        "source": [BACKUP_SOURCE],
        "detail-type": [BACKUP_JOB_STATE_CHANGE],
        "detail": { "state": [FAILED] },
    });
    let put = events(
        &router,
        case,
        region,
        "PutRule",
        &json!({
            "Name": rule,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let rule_arn = member(case, &put, "/RuleArn");
    let (url, queue_arn) = granted_queue(
        &router,
        case,
        region,
        "backup-failed-state-long-poll-queue",
        Some(&rule_arn),
    )
    .await;
    events(
        &router,
        case,
        region,
        "PutTargets",
        &json!({
            "Rule": rule,
            "Targets": [{ "Id": "sink", "Arn": &queue_arn }],
        }),
    )
    .await;

    let table = dynamodb(
        &router,
        case,
        region,
        "CreateTable",
        &json!({
            "TableName": "backup-long-poll-table",
            "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let resource_arn = member(case, &table, "/TableDescription/TableArn");
    let vault = "backup-long-poll-vault";
    backup(
        &router,
        case,
        region,
        "PUT",
        &format!("/backup-vaults/{vault}"),
        Some(&json!({})),
    )
    .await;
    let role = backup_role(&router, "backup-long-poll-role", None, None).await;

    // The consumer begins waiting BEFORE the job is started, so the transition it must be
    // handed is published while that one call is in its wait.
    let receive_body = json!({
        "QueueUrl": &url,
        "MaxNumberOfMessages": 10,
        "WaitTimeSeconds": LONG_POLL_SECONDS,
    });
    let start_body = json!({
        "BackupVaultName": vault,
        "ResourceArn": &resource_arn,
        "IamRoleArn": &role,
        "IdempotencyToken": "backup-long-poll-token",
    });
    let waiting = sqs(&router, case, region, "ReceiveMessage", &receive_body);
    let starting = backup(
        &router,
        case,
        region,
        "PUT",
        "/backup-jobs",
        Some(&start_body),
    );
    let (answered, started) = ::tokio::join!(waiting, starting);
    let job_id = member(case, &started, "/BackupJobId");
    let bodies: Vec<Value> = answered["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|message| message["Body"].as_str().map(str::to_string))
        .filter_map(|body| serde_json::from_str::<Value>(&body).ok())
        .collect();
    let envelopes = job_envelopes(&bodies, &job_id);
    assert!(
        !envelopes.is_empty(),
        "case {case:?}: the waiting receive must be handed the job's {FAILED:?} transition, \
         published while it waited, got {answered} for job {job_id:?}"
    );
    for envelope in envelopes {
        assert_eq!(
            envelope["detail"]["state"],
            json!(FAILED),
            "case {case:?}: only the transitions into {FAILED:?} reach this rule's queue, got \
             {envelope}"
        );
    }
}
