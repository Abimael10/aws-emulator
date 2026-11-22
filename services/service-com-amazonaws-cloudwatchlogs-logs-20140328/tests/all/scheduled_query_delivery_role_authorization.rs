// SPDX-License-Identifier: CC0-1.0

//! A scheduled query's results reach a destination only under the ROLE that destination names
//! — end to end, with the IAM and Amazon S3 emulators mounted beside this service on one
//! engine, so the policy documents read are the ones IAM itself stored and the objects listed
//! are the ones the object store itself holds.
//!
//! RULE: each destination of a scheduled query names the IAM role its delivery is made under
//! (`S3Configuration$roleArn`, "the ARN of the IAM role that grants permissions to write query
//! results to the specified Amazon S3 destination";
//! `LookupTableConfiguration$roleArn`, the role that "grants permissions to create or update
//! the lookup table with query results"), so the delivery is that role's write rather than
//! this service's: it happens only where the permissions in force on the role allow the action
//! the delivery performs on the resource it writes. A role those permissions do not allow
//! writes NOTHING — no object appears under the destination's prefix and no table is created —
//! and `GetScheduledQueryHistory` reports that destination as `ERROR` carrying the refusal,
//! with no `processedIdentifier` at all. The query itself still completes: what the role
//! decides is where the results went, not whether the run happened. Where IAM gives no answer
//! — this account holds no role under that name, a delivery role being routinely provisioned
//! out of band — nothing is withheld, exactly as this service reads a subscription filter's
//! delivery role.
//!
//! The rows span what the rule generalises over: whether the role is allowed the delivery's own
//! action, only a sibling action on the same resource, the same action on another resource, or
//! every action of the service; whether an explicit deny stands beside the grant; whether the
//! role carries any policy at all; and whether IAM holds the named role. The second test reads
//! the same rule off the OTHER destination the model's `DestinationConfiguration` carries, and
//! the third drives the repair path: repointing only the destination's `roleArn` to a role that
//! does hold the grant makes the next run deliver, under the same prefix.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::delivery_identity_authorization::form_encode;
use crate::harness::{now_ms, REGION};

/// The operations under test, and the reads a delivery is proved through.
const CREATE: &str = "CreateScheduledQuery";
const UPDATE: &str = "UpdateScheduledQuery";
const HISTORY: &str = "GetScheduledQueryHistory";
const GET_LOOKUP_TABLE: &str = "GetLookupTable";

/// The access-key id every call here presents; nothing signs, so it only fills the credential
/// scope the Region is read from.
const ACCESS_KEY: &str = "AKIDEXAMPLE";

/// The host a path-style Amazon S3 request names, in the Region every call is signed for.
const S3_HOST: &str = "s3.eu-west-2.amazonaws.com";

/// The log group every case queries, and the stream its events are written to.
const GROUP: &str = "/aws/emulator/scheduled-query-delivery-role";
const STREAM: &str = "runs";

/// The role a scheduled query EXECUTES under, which is not what these cases grade.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/scheduled-query-execution";

/// The bucket every Amazon S3 destination here delivers into: one bucket, one prefix per case,
/// so what a case's own prefix holds is what its delivery wrote.
const BUCKET: &str = "scheduled-query-delivery-role";

/// A bucket no case delivers into, which the row granting the write somewhere else names.
const OTHER_BUCKET: &str = "scheduled-query-delivery-role-elsewhere";

/// The pipeline every case runs: the ERROR events of the window, as rows.
const QUERY_STRING: &str = "fields @timestamp, @message | filter @message like /ERROR/";

/// The messages seeded into the group — two of them ERROR, which is what the pipeline selects.
const MESSAGES: [&str; 3] = [
    "ERROR order 1 failed",
    "INFO order 2 accepted",
    "ERROR order 3 failed",
];

/// The IAM action an Amazon S3 delivery performs, which is the write Amazon S3 authorizes one
/// object under, and the sibling action on the same objects that does not cover it.
const PUT_OBJECT: &str = "s3:PutObject";
const GET_OBJECT: &str = "s3:GetObject";

/// The IAM actions a lookup-table delivery performs: the two calls the member's own
/// documentation names the role's permissions as — "create or update the lookup table with
/// query results" — of which a refresh of a table that is not there yet makes the first.
const CREATE_LOOKUP_TABLE: &str = "logs:CreateLookupTable";
const UPDATE_LOOKUP_TABLE: &str = "logs:UpdateLookupTable";

/// The statuses `GetScheduledQueryHistory` reports a destination's processing under: still
/// owed, delivered, and refused.
const IN_PROGRESS: &str = "IN_PROGRESS";
const COMPLETE: &str = "COMPLETE";
const ERROR: &str = "ERROR";

/// The error class a refused delivery's `errorMessage` leads with.
const ACCESS_DENIED: &str = "[AccessDenied]";

/// How long a case is given to see the run the clock has made due, and how long it waits
/// between two looks. Generous on purpose: the condition — a record with its destination
/// processed — is what is waited for, never a fixed span.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Milliseconds in one hour.
const HOUR: i64 = 60 * 60 * 1_000;

/// This service beside the IAM and Amazon S3 emulators, all on ONE shared engine: the policy
/// documents this service reads are the ones IAM stored, and the objects a delivery writes land
/// in the object store itself.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signing is not the subject here, so requests are unauthenticated; the credential
        // scope is still carried, because it is what names the Region.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// The authorization header naming `service`'s own credential scope.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential={ACCESS_KEY}/20260101/{REGION}/{service}/aws4_request, \
             SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        )
    }

    /// One CloudWatch Logs call: its status and parsed body.
    async fn logs(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("Logs_20140328.{operation}"))
            .header("authorization", Self::authorization("logs"))
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a valid test request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = response.into_body();
        let parsed = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&payload).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One CloudWatch Logs call that must succeed.
    async fn logs_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.logs(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} {body} must succeed, got {status} {parsed}"
        );
        parsed
    }

    /// One `awsQuery` call to IAM that must succeed.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", Self::authorization("iam"))
            .body(bytes::Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: the IAM {action} setup call must succeed, got {status} with {text}"
        );
    }

    /// One path-style Amazon S3 request: its status and body bytes.
    async fn s3(&self, method: &str, path: &str, body: &[u8]) -> (u16, bytes::Bytes) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{S3_HOST}{path}"))
            .header("host", S3_HOST)
            .header("authorization", Self::authorization("s3"))
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, response.into_body())
    }

    /// `PUT /<bucket>` — S3's `CreateBucket`, naming the Region every call here signs for.
    async fn create_bucket(&self, case: &str, bucket: &str) {
        let (status, body) = self
            .s3(
                "PUT",
                &format!("/{bucket}"),
                format!(
                    "<CreateBucketConfiguration \
                     xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                     <LocationConstraint>{REGION}</LocationConstraint>\
                     </CreateBucketConfiguration>"
                )
                .as_bytes(),
            )
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: create bucket {bucket}: got {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// Every key under `prefix`, read through `ListObjectsV2` — which is what shows a delivery
    /// that wrote where it was not allowed to.
    async fn keys_under(&self, case: &str, bucket: &str, prefix: &str) -> Vec<String> {
        let (status, body) = self
            .s3("GET", &format!("/{bucket}?list-type=2&prefix={prefix}"), b"")
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: list {bucket}/{prefix}: got {}",
            String::from_utf8_lossy(&body)
        );
        let document = String::from_utf8_lossy(&body).into_owned();
        document
            .split("<Key>")
            .skip(1)
            .filter_map(|tail| tail.split("</Key>").next())
            .map(str::to_string)
            .collect()
    }
}

/// The IAM role ARN of this account that `name` names. An IAM ARN carries no Region.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// The trust policy every role here carries: CloudWatch Logs may assume it. Being assumable is
/// not what these cases grade — what the role is ALLOWED to do once assumed is.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "logs.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// One identity-based policy document over the statements it is given, each stating one effect,
/// one action and one resource.
fn document(statements: &[(&str, &str, String)]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": statements
            .iter()
            .map(|(effect, action, resource)| json!({
                "Effect": effect,
                "Action": action,
                "Resource": resource,
            }))
            .collect::<Vec<Value>>(),
    })
    .to_string()
}

/// Create the role `name` with `statements` as its one inline policy, when `held`; a row that
/// holds no role creates nothing, so IAM has nothing to answer about it.
async fn given_role(
    emulator: &Emulator,
    case: &str,
    name: &str,
    held: bool,
    statements: &[(&str, &str, String)],
) {
    if !held {
        return;
    }
    emulator
        .iam(
            case,
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust_policy()),
            ],
        )
        .await;
    if statements.is_empty() {
        return;
    }
    emulator
        .iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "delivery".to_string()),
                ("PolicyDocument", document(statements)),
            ],
        )
        .await;
}

/// Seed [`GROUP`] with [`MESSAGES`] an hour back, so every run's lookback window holds them.
async fn seed_events(emulator: &Emulator, case: &str) {
    emulator
        .logs_ok(case, "CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    emulator
        .logs_ok(
            case,
            "CreateLogStream",
            &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
        )
        .await;
    let first_ms = now_ms() - HOUR;
    let events: Vec<Value> = MESSAGES
        .iter()
        .enumerate()
        .map(|(index, message)| {
            json!({"timestamp": first_ms + index as i64 * 1_000, "message": message})
        })
        .collect();
    emulator
        .logs_ok(
            case,
            "PutLogEvents",
            &json!({
                "logGroupName": GROUP,
                "logStreamName": STREAM,
                "logEvents": events,
            }),
        )
        .await;
}

/// The whole history of the scheduled query `name`, newest record first.
async fn history(emulator: &Emulator, case: &str, name: &str) -> Vec<Value> {
    let page = emulator
        .logs_ok(
            case,
            HISTORY,
            &json!({"identifier": name, "startTime": 0, "endTime": now_ms() / 1_000 + 3_600}),
        )
        .await;
    page["triggerHistory"].as_array().cloned().unwrap_or_default()
}

/// The destination of `destination_type` `record` reports, or `None` when it reports none.
fn destination_of(record: &Value, destination_type: &str) -> Option<Value> {
    record["destinations"].as_array().and_then(|destinations| {
        destinations
            .iter()
            .find(|destination| destination["destinationType"].as_str() == Some(destination_type))
            .cloned()
    })
}

/// The record and the destination of `destination_type` the newest run of `name` reports, once
/// that destination has been processed — polled for, because the run that processes it is owed
/// by the clock, and never waited out for a fixed span.
async fn processed_destination(
    emulator: &Emulator,
    case: &str,
    name: &str,
    destination_type: &str,
) -> (Value, Value) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let records = history(emulator, case, name).await;
        let settled = records.first().and_then(|record| {
            destination_of(record, destination_type)
                .filter(|destination| destination["status"].as_str() != Some(IN_PROGRESS))
                .map(|destination| (record.clone(), destination))
        });
        if let Some(settled) = settled {
            return settled;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the {destination_type} destination of {name} to be \
             processed, got {records:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert `destination` is a REFUSED delivery: the status the service reports for one, an
/// `errorMessage` led by the error class and naming both the action that was refused and the
/// role it was refused to, and no `processedIdentifier` at all.
fn assert_refused(case: &str, destination: &Value, action: &str, role: &str) {
    assert_eq!(
        destination["status"].as_str(),
        Some(ERROR),
        "case {case:?}: expected the refused delivery to be {ERROR}, got {destination}"
    );
    let message = destination["errorMessage"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a refused delivery says why, got {destination}")
        })
        .to_string();
    assert!(
        message.starts_with(ACCESS_DENIED),
        "case {case:?}: expected the refusal to lead with {ACCESS_DENIED}, got {message:?}"
    );
    assert!(
        message.contains(action),
        "case {case:?}: expected the refusal to name {action}, got {message:?}"
    );
    assert!(
        message.contains(role),
        "case {case:?}: expected the refusal to name the role {role}, got {message:?}"
    );
    assert!(
        destination.get("processedIdentifier").is_none(),
        "case {case:?}: a delivery that did not happen produced nothing, got {destination}"
    );
}

/// Assert the run `record` reports COMPLETED: the role decides where the results went, not
/// whether the query ran.
fn assert_run_completed(case: &str, record: &Value) {
    assert_eq!(
        record["executionStatus"].as_str(),
        Some("Complete"),
        "case {case:?}: the query itself still completes, got {record}"
    );
}

/// One row: the documents IAM holds for the delivery role, and whether the object must land.
struct Row {
    case: &'static str,
    /// The row's own suffix: its role, its scheduled query and its destination prefix are all
    /// named from it, so every row is isolated from every other.
    name: &'static str,
    /// Whether IAM holds a role under that name at all.
    held: bool,
    /// The statements of the role's one inline policy.
    statements: Vec<(&'static str, &'static str, String)>,
    /// Whether the object must be written at the destination.
    delivers: bool,
}

fn rows() -> Vec<Row> {
    let every_object = format!("arn:aws:s3:::{BUCKET}/*");
    let elsewhere = format!("arn:aws:s3:::{OTHER_BUCKET}/*");
    vec![
        Row {
            case: "a role allowed the write on every object of the destination bucket",
            name: "allowed",
            held: true,
            statements: vec![("Allow", PUT_OBJECT, every_object.clone())],
            delivers: true,
        },
        // The recorded case: the delivery role is allowed only to READ the objects it is
        // pointed at.
        Row {
            case: "a role allowed only the read of those same objects",
            name: "read-only",
            held: true,
            statements: vec![("Allow", GET_OBJECT, every_object.clone())],
            delivers: false,
        },
        Row {
            case: "a role allowed the write on ANOTHER bucket's objects",
            name: "other-bucket",
            held: true,
            statements: vec![("Allow", PUT_OBJECT, elsewhere)],
            delivers: false,
        },
        Row {
            case: "a role allowed every action of the service on every resource",
            name: "wildcard",
            held: true,
            statements: vec![("Allow", "s3:*", "*".to_string())],
            delivers: true,
        },
        Row {
            case: "a grant and an explicit deny of the same write",
            name: "allow-and-deny",
            held: true,
            statements: vec![
                ("Allow", PUT_OBJECT, every_object.clone()),
                ("Deny", PUT_OBJECT, every_object),
            ],
            delivers: false,
        },
        Row {
            case: "a role carrying no identity policy at all",
            name: "no-policy",
            held: true,
            statements: Vec::new(),
            delivers: false,
        },
        // Nothing is known about a role this account's IAM holds nothing under, so nothing is
        // withheld on its account.
        Row {
            case: "a role IAM holds nothing under",
            name: "absent-from-iam",
            held: false,
            statements: Vec::new(),
            delivers: true,
        },
    ]
}

/// The `s3://` URI the row's destination names: one prefix per row inside the one bucket.
fn destination_uri(name: &str) -> String {
    format!("s3://{BUCKET}/{name}/")
}

/// An Amazon S3 delivery happens only where the destination's own role is allowed the write,
/// and a refused one writes nothing and says why.
#[tokio::test]
async fn an_amazon_s3_delivery_happens_only_under_a_role_allowed_the_write() {
    let emulator = Emulator::new();
    seed_events(&emulator, "seed").await;
    emulator.create_bucket("seed", BUCKET).await;

    let rows = rows();
    for row in &rows {
        let case = row.case;
        let role = format!("sq-delivery-{}", row.name);
        given_role(&emulator, case, &role, row.held, &row.statements).await;
        emulator
            .logs_ok(
                case,
                CREATE,
                &json!({
                    "name": row.name,
                    "queryLanguage": "CWLI",
                    "queryString": QUERY_STRING,
                    "scheduleExpression": "rate(1 day)",
                    "executionRoleArn": EXECUTION_ROLE,
                    "logGroupIdentifiers": [GROUP],
                    "startTimeOffset": 7_200,
                    "destinationConfiguration": {
                        "s3Configuration": {
                            "destinationIdentifier": destination_uri(row.name),
                            "roleArn": role_arn(&role),
                        },
                    },
                }),
            )
            .await;
    }

    for row in &rows {
        let case = row.case;
        let role = format!("sq-delivery-{}", row.name);
        let (record, destination) = processed_destination(&emulator, case, row.name, "S3").await;
        assert_run_completed(case, &record);
        assert_eq!(
            destination["destinationIdentifier"].as_str(),
            Some(destination_uri(row.name).as_str()),
            "case {case:?}: the entry names the destination it was configured with, got \
             {destination}"
        );
        let keys = emulator.keys_under(case, BUCKET, row.name).await;
        if !row.delivers {
            assert_refused(case, &destination, PUT_OBJECT, &role);
            assert!(
                keys.is_empty(),
                "case {case:?}: a refused delivery writes nothing, got {keys:?}"
            );
            continue;
        }
        assert_eq!(
            destination["status"].as_str(),
            Some(COMPLETE),
            "case {case:?}: expected the allowed delivery to complete, got {destination}"
        );
        let uri = destination["processedIdentifier"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a processed delivery names what it produced, got {destination}")
            })
            .to_string();
        let key = uri
            .strip_prefix(&format!("s3://{BUCKET}/"))
            .unwrap_or_else(|| panic!("case {case:?}: the delivered object is in {BUCKET}, got {uri:?}"))
            .to_string();
        assert_eq!(
            keys,
            vec![key.clone()],
            "case {case:?}: expected the prefix to hold exactly {key:?}, got {keys:?}"
        );
    }
}

/// The same rule at the OTHER destination the model's `DestinationConfiguration` carries: the
/// lookup table is created only where the destination's role is allowed the call the refresh
/// makes, and a refused refresh creates no table.
#[tokio::test]
async fn a_lookup_table_delivery_happens_only_under_a_role_allowed_the_refresh() {
    let emulator = Emulator::new();
    seed_events(&emulator, "seed").await;

    // (case, the query's name, the table it refreshes, the action its role is allowed, whether
    // the table must be created)
    let cases: [(&str, &str, &str, &str, bool); 3] = [
        (
            "a role allowed to create the table the refresh creates",
            "table-allowed",
            "sq_table_allowed",
            CREATE_LOOKUP_TABLE,
            true,
        ),
        (
            "a role allowed only to update a table that is not there yet",
            "table-update-only",
            "sq_table_update_only",
            UPDATE_LOOKUP_TABLE,
            false,
        ),
        (
            "a role allowed neither, holding an unrelated grant",
            "table-unrelated",
            "sq_table_unrelated",
            "logs:DescribeLogGroups",
            false,
        ),
    ];

    for (case, name, table, action, _) in cases {
        let role = format!("sq-table-{name}");
        let table_arn =
            service_com_amazonaws_cloudwatchlogs_logs_20140328::state::lookup_table_arn(
                REGION, table,
            );
        given_role(
            &emulator,
            case,
            &role,
            true,
            &[("Allow", action, table_arn)],
        )
        .await;
        emulator
            .logs_ok(
                case,
                CREATE,
                &json!({
                    "name": name,
                    "queryLanguage": "CWLI",
                    "queryString": QUERY_STRING,
                    "scheduleExpression": "rate(1 day)",
                    "executionRoleArn": EXECUTION_ROLE,
                    "logGroupIdentifiers": [GROUP],
                    "startTimeOffset": 7_200,
                    "destinationConfiguration": {
                        "lookupTableConfiguration": {
                            "tableName": table,
                            "roleArn": role_arn(&role),
                        },
                    },
                }),
            )
            .await;
    }

    for (case, name, table, _, refreshes) in cases {
        let role = format!("sq-table-{name}");
        let table_arn =
            service_com_amazonaws_cloudwatchlogs_logs_20140328::state::lookup_table_arn(
                REGION, table,
            );
        let (record, destination) =
            processed_destination(&emulator, case, name, "LOOKUP_TABLE").await;
        assert_run_completed(case, &record);
        let (status, read) = emulator
            .logs(GET_LOOKUP_TABLE, &json!({"lookupTableArn": table_arn}))
            .await;
        if !refreshes {
            assert_refused(case, &destination, CREATE_LOOKUP_TABLE, &role);
            assert_ne!(
                status, 200,
                "case {case:?}: a refused refresh creates no table, got {status} {read}"
            );
            continue;
        }
        assert_eq!(
            destination["status"].as_str(),
            Some(COMPLETE),
            "case {case:?}: expected the allowed refresh to complete, got {destination}"
        );
        assert_eq!(
            status, 200,
            "case {case:?}: the refreshed table is readable, got {status} {read}"
        );
        assert_eq!(
            read["lookupTableName"].as_str(),
            Some(table),
            "case {case:?}: the delivered table is the one the destination names, got {read}"
        );
        let body = read["tableBody"].as_str().unwrap_or_default().to_string();
        for message in MESSAGES.iter().filter(|message| message.contains("ERROR")) {
            assert!(
                body.contains(message),
                "case {case:?}: expected the refreshed rows to hold {message:?}, got {body:?}"
            );
        }
    }
}

/// The authorization decision is taken PER DELIVERY: repointing only the destination's
/// `roleArn` to a role that does hold the grant makes the next run deliver, under the same
/// prefix the refused one wrote nothing under.
#[tokio::test]
async fn repointing_the_destination_role_makes_the_next_run_deliver() {
    const CASE: &str = "a destination repointed from a refused role to an allowed one";
    const NAME: &str = "repaired";
    let emulator = Emulator::new();
    seed_events(&emulator, CASE).await;
    emulator.create_bucket(CASE, BUCKET).await;

    let every_object = format!("arn:aws:s3:::{BUCKET}/*");
    let refused = "sq-repair-refused";
    let allowed = "sq-repair-allowed";
    given_role(
        &emulator,
        CASE,
        refused,
        true,
        &[("Allow", GET_OBJECT, every_object.clone())],
    )
    .await;
    given_role(
        &emulator,
        CASE,
        allowed,
        true,
        &[("Allow", PUT_OBJECT, every_object)],
    )
    .await;

    // A rate of one minute, the shortest period a schedule expression states, so the run after
    // the refused one falls due on its own.
    let configuration = |role: &str| {
        json!({
            "queryLanguage": "CWLI",
            "queryString": QUERY_STRING,
            "scheduleExpression": "rate(1 minute)",
            "executionRoleArn": EXECUTION_ROLE,
            "logGroupIdentifiers": [GROUP],
            "startTimeOffset": 7_200,
            "destinationConfiguration": {
                "s3Configuration": {
                    "destinationIdentifier": destination_uri(NAME),
                    "roleArn": role_arn(role),
                },
            },
        })
    };
    let mut create = configuration(refused);
    create["name"] = json!(NAME);
    emulator.logs_ok(CASE, CREATE, &create).await;

    let (_, destination) = processed_destination(&emulator, CASE, NAME, "S3").await;
    assert_refused(CASE, &destination, PUT_OBJECT, refused);
    let keys = emulator.keys_under(CASE, BUCKET, NAME).await;
    assert!(
        keys.is_empty(),
        "case {CASE:?}: the refused run wrote nothing, got {keys:?}"
    );
    let refused_runs = history(&emulator, CASE, NAME).await.len();

    // Only the destination's `roleArn` is repointed; everything else is restated as it stood.
    let mut update = configuration(allowed);
    update["identifier"] = json!(NAME);
    emulator.logs_ok(CASE, UPDATE, &update).await;

    // The next run the schedule names, polled for: its delivery is decided afresh, under the
    // role the destination now carries.
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let delivered = loop {
        let records = history(&emulator, CASE, NAME).await;
        let newest = records
            .first()
            .filter(|_| records.len() > refused_runs)
            .and_then(|record| destination_of(record, "S3"))
            .filter(|destination| destination["status"].as_str() != Some(IN_PROGRESS));
        if let Some(newest) = newest {
            break newest;
        }
        assert!(
            Instant::now() < deadline,
            "case {CASE:?}: expected a run past the repointed role to settle, got {records:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        delivered["status"].as_str(),
        Some(COMPLETE),
        "case {CASE:?}: expected the run under the allowed role to deliver, got {delivered}"
    );
    let uri = delivered["processedIdentifier"]
        .as_str()
        .unwrap_or_else(|| panic!("case {CASE:?}: a processed delivery names what it produced, got {delivered}"))
        .to_string();
    let expected_prefix = format!("s3://{BUCKET}/{NAME}/");
    assert!(
        uri.starts_with(&expected_prefix),
        "case {CASE:?}: expected the object under {expected_prefix:?}, got {uri:?}"
    );
    let key = uri
        .strip_prefix(&format!("s3://{BUCKET}/"))
        .unwrap_or_default()
        .to_string();
    let keys = emulator.keys_under(CASE, BUCKET, NAME).await;
    assert!(
        keys.contains(&key),
        "case {CASE:?}: expected the object store to hold {key:?}, got {keys:?}"
    );
}
