// SPDX-License-Identifier: CC0-1.0

//! An EventBridge rule's `detail-type` clause SCOPES which of AWS Backup's own events reach
//! that rule's queue: the envelope of the kind of record that moved reaches the rule that
//! names that kind, and a sibling rule on the same bus naming another kind receives none of
//! it.
//!
//! The RULE pinned here: **AWS Backup publishes each state a record of its own MOVES INTO
//! onto the account's default EventBridge bus under its own `source` (`aws.backup`) and the
//! `detail-type` of the KIND of record that moved — a backup job's moves under
//! `Backup Job State Change`, the recovery point that backup WROTE under
//! `Recovery Point State Change` — and EventBridge routes each envelope by its CONTENT: a
//! rule receives an envelope only where every clause of its `EventPattern` matches, so a
//! rule naming one `detail-type` receives the records of that kind alone, a rule naming
//! several receives every kind it lists, a rule naming the source alone receives every kind
//! that source publishes, and a rule naming another kind — or the same words in another
//! letter case, since a pattern's values match exactly — receives nothing of it, however
//! many of its siblings on the same bus do.** The state the START call stored is not a move:
//! it is what that call's own response reports, so the envelopes the bus is owed are the
//! transitions that follow it.
//!
//! Source for the envelope: the AWS Backup EventBridge notifications, which carry
//! `"source": "aws.backup"` with the `detail-type` of the kind of record that moved; the
//! backup Smithy model describes the control plane, not the bus, so the `detail` members are
//! asserted against what the SAME job's control-plane response reports for them, and the
//! states a job can take against `smithy_query(query="shape",
//! shape="com.amazonaws.backup#BackupJobState")`. Those two detail-types are spelled out here
//! rather than imported from the service crate, so a wrong constant in the implementation
//! cannot make this suite pass.
//!
//! Five crates have to agree for that to hold — Amazon DynamoDB holds the protected table,
//! IAM holds the role the backup is performed under, AWS Backup runs the job and publishes
//! its move, EventBridge matches each envelope against each rule's pattern, and Amazon SQS
//! holds what arrived under its own queue policy — so the suite lives here rather than in any
//! one crate, and it runs over the WHOLE `build_router` service set the binary serves.
//!
//! The rows differ along exactly what the rule generalises over: the `detail-type` clause of
//! the rule's pattern (the moved record's own kind, a list naming that kind among others, no
//! clause at all, another kind of record, and the moved record's kind in another letter case)
//! — and so along whether the rule's own queue receives the job's envelope. ONE backup job's
//! transition is published for every row, so the rows that must receive NOTHING are read
//! against a POSITIVE CONTROL rather than a bare quiet window: the rows that do receive it
//! are what say the envelope really was published and dispatched.
//!
//! What is NOT asserted here, because it is asserted elsewhere:
//! `services/service-com-amazonaws-backup-cryocontrollerusermanager/tests/all/job_state_change_events.rs`
//! pins the published `detail` of every job KIND against the control plane's own reads,
//! `crate::cfn_deployed_backup_pipeline_delivers_job_events` pins the SNS hop and the
//! template-provisioned pipeline, and `crate::backup_job_state_change_invokes_its_lambda_target`
//! pins the same publication routed to a LAMBDA target. What only this suite reaches is the
//! SCOPING itself: several rules, differing only in their `detail-type` clause, reading one
//! real job's one transition off their own queues.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery IAM caller, the XML reader and the Amazon SQS caller this test crate writes
// ONCE rather than per suite. They sign for the Region below, which is why this suite signs
// for it too.
use crate::query_wire::element;
use crate::sns_lambda_subscription_permission_boundary::{drain, iam, sqs};

/// The Region every request below is signed for — the one the shared callers sign for.
const REGION: &str = "us-east-1";

/// The `source` AWS Backup publishes under, and the two `detail-type`s a backup job's own
/// transition and the recovery point it wrote are published with.
const BACKUP_SOURCE: &str = "aws.backup";
const BACKUP_JOB_STATE_CHANGE: &str = "Backup Job State Change";
const RECOVERY_POINT_STATE_CHANGE: &str = "Recovery Point State Change";

/// Two other kinds of record this service publishes, named by rows whose point is the clause
/// rather than the kind: one the row lists BESIDE the job's own kind, one it names INSTEAD.
const COPY_JOB_STATE_CHANGE: &str = "Copy Job State Change";
const RESTORE_JOB_STATE_CHANGE: &str = "Restore Job State Change";

/// The service principal EventBridge delivers to a queue as, and the write it makes.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The state a backup job is stored in by the call that starts it, and the terminal state a
/// job performed under a permitted role reaches — the move the bus is owed.
const CREATED: &str = "CREATED";
const COMPLETED: &str = "COMPLETED";

/// Every state a backup job may be IN, as the model's `BackupJobState` declares them
/// (`smithy_query(query="shape", shape="com.amazonaws.backup#BackupJobState")`), so a
/// published state is checked against the catalog rather than against the one this job takes:
/// a job that moves through more of them than this one does still publishes only these.
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

/// How long a row waits for the delivery it must see, how long the rows that must see NOTHING
/// keep looking for one, and how often either looks. Each is generous: nothing here is a
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

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: the router above runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
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

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`), which must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("authorization", authorization("events"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(&router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon DynamoDB call (`awsJson1_0`, `DynamoDB_20120810.<Operation>`), which must
/// answer 200.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("authorization", authorization("dynamodb"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(&router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One AWS Backup call (`restJson1`), which must answer 200.
async fn backup(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Value {
    let payload = body.map_or_else(Bytes::new, |body| Bytes::from(body.to_string()));
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://backup.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("backup.{REGION}.amazonaws.com"))
        .header("authorization", authorization("backup"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(payload)
        .expect("the test request is well formed");
    let (status, parsed) = answered(&router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Backup {method} {uri}, got {status} with {parsed}"
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

/// A queue whose own policy admits the write EventBridge makes to it ("for … Amazon SQS
/// queues, EventBridge relies on resource-based policies", `PutTargets`), so what reaches it
/// is decided by the rule's pattern rather than by the queue's document. Answers its url and
/// the ARN a target names it by, read back from the queue itself.
async fn granted_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = member(case, &created, "/QueueUrl");
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = member(case, &attributes, "/Attributes/QueueArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": SEND_MESSAGE_ACTION,
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// The trust document of a role a service may act as.
fn trusting(service: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create the role `name` where IAM holds it, trusted by `service` and allowed everything a
/// backup needs, and answer the ARN IAM ITSELF reports for it.
async fn backup_role(router: &MultiServiceRouter, name: &str, service: &str) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trusting(service)),
        ],
    )
    .await;
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": "*", "Resource": "*" }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "granted"),
            ("PolicyDocument", &policy),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// One row: the `detail-type` clause the rule's pattern carries, and what its own queue must
/// therefore receive of the job's transition.
struct Row {
    case: &'static str,
    /// The values of the pattern's `detail-type` clause. An EMPTY list writes no clause at
    /// all, so the pattern names the source alone.
    detail_types: &'static [&'static str],
    /// Whether the job's own `Backup Job State Change` envelope must reach this row's queue.
    receives_job: bool,
    /// Whether the recovery point the backup WROTE must reach it too — true only for a
    /// pattern that admits every kind this source publishes.
    receives_recovery_point: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the detail-type of the kind of record that moved",
        detail_types: &[BACKUP_JOB_STATE_CHANGE],
        receives_job: true,
        receives_recovery_point: false,
    },
    Row {
        case: "a list naming that kind among others",
        detail_types: &[COPY_JOB_STATE_CHANGE, BACKUP_JOB_STATE_CHANGE],
        receives_job: true,
        receives_recovery_point: false,
    },
    Row {
        case: "the source alone, with no detail-type clause",
        detail_types: &[],
        receives_job: true,
        receives_recovery_point: true,
    },
    Row {
        case: "another kind of record's detail-type",
        detail_types: &[RESTORE_JOB_STATE_CHANGE],
        receives_job: false,
        receives_recovery_point: false,
    },
    Row {
        case: "the moved record's kind in another letter case",
        detail_types: &["backup job state change"],
        receives_job: false,
        receives_recovery_point: false,
    },
];

/// Every envelope the queue at `url` has received, parsed, accumulating across looks.
async fn received(router: &MultiServiceRouter, seen: &mut Vec<Value>, url: &str) {
    for body in drain(router, url).await {
        if let Ok(envelope) = serde_json::from_str::<Value>(&body) {
            seen.push(envelope);
        }
    }
}

/// Whether `seen` holds an envelope of kind `detail_type` naming `job_id` in its detail.
fn holds(seen: &[Value], detail_type: &str, job_id: &str) -> bool {
    seen.iter().any(|envelope| {
        envelope["detail-type"] == json!(detail_type) && envelope.to_string().contains(job_id)
    })
}

/// RULE: one backup job's one transition reaches exactly the rules whose `detail-type` clause
/// admits the kind of record that moved.
#[tokio::test]
async fn backup_events_reach_the_rules_whose_detail_type_admits_them() {
    let router = router();
    let case = "the shared job every row reads";

    // The rules and their queues FIRST, so every row is watching before anything is
    // published: a rule installed after a publication has nothing to be handed.
    let mut queues: Vec<String> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let (url, queue_arn) =
            granted_queue(&router, row.case, &format!("backup-scope-queue-{index}")).await;
        let mut pattern = json!({ "source": [BACKUP_SOURCE] });
        if !row.detail_types.is_empty() {
            pattern["detail-type"] = json!(row.detail_types);
        }
        let rule_name = format!("backup-scope-rule-{index}");
        events(
            &router,
            row.case,
            "PutRule",
            &json!({
                "Name": &rule_name,
                "EventPattern": pattern.to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let wired = events(
            &router,
            row.case,
            "PutTargets",
            &json!({
                "Rule": &rule_name,
                "Targets": [{ "Id": "sink", "Arn": &queue_arn }],
            }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {:?}: the queue target must be accepted, got {wired}",
            row.case
        );
        queues.push(url);
    }

    // ONE real job, whose ONE transition every row's rule is matched against: the table it
    // protects is held by the service that owns it, the vault by AWS Backup, and the role by
    // IAM — which is what leads the job to its terminal state.
    let table = dynamodb(
        &router,
        case,
        "CreateTable",
        &json!({
            "TableName": "backup-scope-table",
            "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let resource_arn = member(case, &table, "/TableDescription/TableArn");
    let vault = "backup-scope-vault";
    backup(
        &router,
        case,
        "PUT",
        &format!("/backup-vaults/{vault}"),
        Some(&json!({})),
    )
    .await;
    let role = backup_role(&router, "backup-scope-role", "backup.amazonaws.com").await;
    let started = backup(
        &router,
        case,
        "PUT",
        "/backup-jobs",
        Some(&json!({
            "BackupVaultName": vault,
            "ResourceArn": &resource_arn,
            "IamRoleArn": &role,
        })),
    )
    .await;
    let job_id = member(case, &started, "/BackupJobId");
    let recovery_point_arn = member(case, &started, "/RecoveryPointArn");

    // Every row reads its OWN queue, looking again until the rows that must receive the
    // envelope have it. The rows that must receive it are the positive control for the ones
    // that must not: a queue still empty once they hold the job's envelope is the pattern's
    // refusal rather than a look taken too early.
    let mut seen: Vec<Vec<Value>> = vec![Vec::new(); ROWS.len()];
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        for (index, url) in queues.iter().enumerate() {
            received(&router, &mut seen[index], url).await;
        }
        let delivered = ROWS.iter().enumerate().all(|(index, row)| {
            !row.receives_job || holds(&seen[index], BACKUP_JOB_STATE_CHANGE, &job_id)
        });
        if delivered {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every rule whose pattern admits the job's kind must receive its \
             envelope, got {seen:?} for job {job_id:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    // The envelope the admitting rows received is the job's own transition, in full.
    for (index, row) in ROWS.iter().enumerate() {
        if !row.receives_job {
            continue;
        }
        let envelopes: Vec<&Value> = seen[index]
            .iter()
            .filter(|envelope| envelope["detail-type"] == json!(BACKUP_JOB_STATE_CHANGE))
            .collect();
        // EACH state the job moves into is published once, so the kind's envelopes carry no
        // state twice and each is a state the model declares — however many transitions this
        // job takes on its way to its terminal one. The state the START call stored is not a
        // move, so the state that must be among them is the one it moved INTO.
        let mut states: Vec<String> = envelopes
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
            states.windows(2).all(|pair| pair[0] != pair[1]),
            "case {:?}: a state is published once, so none may reach the queue twice, got \
             {states:?} in {:?}",
            row.case,
            seen[index]
        );
        assert!(
            states.iter().all(|state| JOB_STATES.contains(&state.as_str())),
            "case {:?}: every published state is one the model declares, got {states:?}",
            row.case
        );
        let terminal: Vec<&&Value> = envelopes
            .iter()
            .filter(|envelope| envelope["detail"]["state"] == json!(COMPLETED))
            .collect();
        assert_eq!(
            terminal.len(),
            1,
            "case {:?}: expected exactly one envelope carrying {COMPLETED:?}, got {states:?} \
             in {:?}",
            row.case,
            seen[index]
        );
        let envelope = terminal[0];
        for (pointer, expect) in [
            ("/source", json!(BACKUP_SOURCE)),
            ("/detail-type", json!(BACKUP_JOB_STATE_CHANGE)),
            ("/region", json!(REGION)),
            ("/resources", json!([&recovery_point_arn])),
            ("/detail/backupJobId", json!(&job_id)),
            ("/detail/state", json!(COMPLETED)),
            ("/detail/backupVaultName", json!(vault)),
            ("/detail/resourceArn", json!(&resource_arn)),
            ("/detail/iamRoleArn", json!(&role)),
        ] {
            let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expect,
                "case {:?}: expected {pointer} to be {expect}, got {actual} in {envelope}",
                row.case
            );
        }
    }

    // A row whose pattern admits every kind this source publishes receives the RECOVERY POINT
    // the backup wrote as well, under that kind's own detail-type; one scoped to the job's
    // kind receives only the job.
    for (index, row) in ROWS.iter().enumerate() {
        let kinds: BTreeSet<String> = seen[index]
            .iter()
            .filter_map(|envelope| envelope["detail-type"].as_str())
            .map(str::to_string)
            .collect();
        assert_eq!(
            kinds.contains(RECOVERY_POINT_STATE_CHANGE),
            row.receives_recovery_point,
            "case {:?}: expected receives_recovery_point={}, got the kinds {kinds:?}",
            row.case,
            row.receives_recovery_point
        );
    }

    // And the rows the pattern refuses received nothing of the job, over a window longer than
    // the span between two dispatches of one publication.
    let quiet_until = Instant::now() + QUIET_WINDOW;
    while Instant::now() < quiet_until {
        for (index, url) in queues.iter().enumerate() {
            received(&router, &mut seen[index], url).await;
            let row = &ROWS[index];
            if row.receives_job {
                continue;
            }
            assert!(
                !seen[index].iter().any(|envelope| envelope
                    .to_string()
                    .contains(&job_id)),
                "case {:?}: nothing of the job may reach this rule's queue, got {:?}",
                row.case,
                seen[index]
            );
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    // The state the envelope carried is the state AWS Backup's own reader reports for the
    // same job: the envelope is the job's stored state rather than a second copy of it.
    let read = backup(
        &router,
        case,
        "GET",
        &format!("/backup-jobs/{job_id}"),
        None,
    )
    .await;
    let reported = member(case, &read, "/State");
    assert_eq!(
        reported, COMPLETED,
        "case {case:?}: expected DescribeBackupJob to report {COMPLETED:?} for the state the \
         envelope carried, got {reported:?} (and never {CREATED:?} once it has moved) in {read}"
    );
}
