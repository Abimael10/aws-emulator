// SPDX-License-Identifier: CC0-1.0

//! The two Glue-driven paths a PERMISSION BOUNDARY is observed through come to rest on the
//! ASSEMBLED emulator, and what they come to rest in is decided by the run role's own trust
//! document and the delivery target's own resource policy.
//!
//! The RULES under test:
//!
//! 1. **A Glue job run executes its script AS the job's `Role`, so the trust document that role
//!    carries decides the run: a role whose document admits the Glue service principal runs to
//!    the terminal `SUCCEEDED` leaving behind what its script wrote, and a role whose document
//!    admits only some OTHER principal — or admits Glue for some other action — backs no run at
//!    all, so the run comes to rest `FAILED` carrying a `JobRun$ErrorMessage` that states the
//!    assume-role requirement and names the role, with nothing written where the script would
//!    have written.** Sources: `Job$Role` ("the name or Amazon Resource Name (ARN) of the IAM
//!    role associated with this job"), `JobCommand$ScriptLocation`, `JobRun$ErrorMessage` ("an
//!    error message associated with this job run") and the `JobRunState` enumeration of the Glue
//!    model.
//!
//! 2. **A job run that comes to rest is published once onto the account's DEFAULT EventBridge
//!    bus as `source` `aws.glue` / `detail-type` `Glue Job State Change`, carrying the run's own
//!    `jobName`, `state` and `jobRunId` in the event `detail`; EventBridge hands the envelope to
//!    every target of each rule whose `EventPattern` matches it, but a queue target receives it
//!    only where that QUEUE's own resource policy admits `sqs:SendMessage` to the EventBridge
//!    service principal — so a permitted queue holds the envelope and an otherwise identical
//!    sibling target of the same rule, with no policy, holds nothing.** Sources: `PutTargets`
//!    ("for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on
//!    resource-based policies") and `PutRule$EventPattern`.
//!
//! WHY this suite is here rather than in one service's crate: each rule is pinned over a wide
//! table by the crate that owns it — `job_run_positive_control.rs` and `job_run_execution_role.rs`
//! in the Glue crate for the run, `cfn_deployed_job_state_rule_to_sqs.rs` beside them and
//! `queue_target_policy_authorization.rs` in the EventBridge crate for the publication and the
//! queue policy. What none of them can reach is the axis a CLIENT of the served emulator drives:
//! both paths over the WHOLE mounted service set, in one process, on one engine — where three
//! mounted models resolve to the `events` route key, so the in-process publication has to be
//! claimed by the one a client's own `events`-scoped call reaches, and where the role records, the
//! object store, the token service, the bus and the queues are all the ones the client itself
//! wrote through.
//!
//! The THIRD path the same group of checks waits on — an S3 Batch Operations job settling by the
//! trust document its `RoleArn` carries — is already pinned on exactly this axis, over a wider
//! table of trust documents and with request authentication on, by
//! `core/router/tests/all/zz_batch_job_probe.rs`; this suite therefore leaves it alone rather
//! than keeping a second copy of it.
//!
//! The rows of each table differ along what its rule generalises over: the trust document's
//! principal and the form it is written in (rule 1), and the dimension of the envelope the rule's
//! pattern filters on (rule 2). Every row is otherwise identical — the same script, the same
//! identity grants, the same attached managed policy — so the only thing a row's outcome can
//! follow from is the one thing it varies.
//!
//! Nothing here sleeps for a span or asserts on the wall clock: every wait is for the CONDITION it
//! is about — the run's state, the message on the queue — looked at again until it holds or a
//! generous deadline passes.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so each test builds it on a
//! sized thread of its own, with a multi-thread runtime, because a job script's execution runs on
//! a thread of its own and dispatches its own AWS calls back onto that runtime.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is addressed to.
const REGION: &str = "us-west-2";

/// The account whose default bus the publication goes onto: the one this emulator serves.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The XML namespace Amazon S3's own model declares, which a client's payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal a Glue job run's role has to admit for the run to be performed, written
/// out here rather than imported from the service crate, so a wrong constant in the
/// implementation cannot make this suite pass.
const GLUE_PRINCIPAL: &str = "glue.amazonaws.com";

/// The action a trust document names for that principal.
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The AWS-managed policy a client attaches to a Glue job's role, as the workflows this suite
/// mirrors do: it is attached on every row, so no row's outcome can follow from the attachment.
const GLUE_SERVICE_ROLE_POLICY: &str = "arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole";

/// The `source` and `detail-type` a Glue job run's state change is published under.
const GLUE_EVENT_SOURCE: &str = "aws.glue";
const JOB_STATE_CHANGE: &str = "Glue Job State Change";

/// The bus an AWS service's own events go onto, which is the bus a rule carrying no
/// `EventBusName` is created on.
const DEFAULT_BUS: &str = "default";

/// The service principal EventBridge delivers to a queue target as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The `JobRunState` values a Glue run never leaves again
/// (`smithy_query(query="shape",shape="JobRunState",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`).
const TERMINAL_RUN_STATES: [&str; 6] = [
    "STOPPED",
    "SUCCEEDED",
    "FAILED",
    "TIMEOUT",
    "ERROR",
    "EXPIRED",
];

/// How long a wait for one condition lasts, and how often the condition is looked at. Generous:
/// what is waited for is the condition rather than a span, so a loaded or a fast host reaches the
/// same verdict.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const DELIVERY_DEADLINE: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// How long a row leaves the queue that granted EventBridge nothing alone before reading it: the
/// read is taken only AFTER the permitted queue has received its own copy of the same envelope,
/// so an empty answer is the policy withholding the delivery rather than a look taken too early;
/// this is the further span on top of that.
const QUIET_SPAN: Duration = Duration::from_secs(2);

/// The script every row runs: it writes a marker object at the key its own arguments name, which
/// is the one call the run makes of its own.
const WRITES_ITS_MARKER: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
boto3.client("s3").put_object(
    Bucket=named["--bucket"], Key=named["--marker_key"], Body=b"marker"
)
"#;

/// The body the script writes, which is what a marker that really was written holds.
const MARKER_BODY: &str = "marker";

/// The sentence a run that was never dispatched carries, as Glue words the assume-role
/// requirement for this class: fixed text with ONE variable part, the role the job names.
fn assume_role_requirement(role_arn: &str) -> String {
    format!("Role {role_arn} should be given assume role permissions for Glue Service.")
}

/// What one call answered.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// The body as JSON, or `Value::Null` for one that is not JSON.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator this suite drives: the whole mounted service set over ONE engine, exactly as
/// `core/router/src/lib.rs` assembles it for the binary.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    /// Build it. Request authentication is off: this suite exercises the services, so each call
    /// carries a credential scope that names the service it is for and no computed signature.
    fn start() -> Self {
        let engine = aws_emulator_router::new_engine();
        let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
        router.set_request_authentication(false);
        Self { router }
    }

    /// Dispatch one request as it arrives on the wire.
    async fn send(
        &self,
        method: &str,
        host: &str,
        signing_name: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Answer {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDBOUNDARY/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=not-verified"
                ),
            );
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(body.to_owned()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        Answer {
            status: response.status().as_u16(),
            body: String::from_utf8_lossy(response.body()).into_owned(),
        }
    }

    /// One Amazon S3 REST call, path-style as the emulator serves it.
    async fn s3(&self, method: &str, path: &str, body: &str) -> Answer {
        let host = format!("s3.{REGION}.amazonaws.com");
        self.send(
            method,
            &host,
            "s3",
            path,
            &[("content-type", "application/octet-stream")],
            body,
        )
        .await
    }

    /// One IAM `awsQuery` action, which must be served.
    async fn iam(&self, case: &str, action: &str, members: &[(&str, &str)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in members {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let answered = self
            .send(
                "POST",
                "iam.amazonaws.com",
                "iam",
                "/",
                &[("content-type", "application/x-www-form-urlencoded")],
                &body,
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: IAM {action} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered.body
    }

    /// One `X-Amz-Target` RPC call, which must be served.
    async fn rpc(
        &self,
        case: &str,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> Value {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let answered = self
            .send(
                "POST",
                &host,
                signing_name,
                "/",
                &[
                    (
                        "content-type",
                        &format!("application/x-amz-json-{json_version}"),
                    ),
                    ("x-amz-target", &format!("{target_prefix}.{operation}")),
                ],
                &body.to_string(),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: {signing_name} {operation} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered.json()
    }

    /// One AWS Glue call (`awsJson1_1`, `AWSGlue.<Operation>`).
    async fn glue(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "glue", "AWSGlue", "1.1", operation, body)
            .await
    }

    /// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`).
    async fn events(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "events", "AWSEvents", "1.1", operation, body)
            .await
    }

    /// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`).
    async fn sqs(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "sqs", "AmazonSQS", "1.0", operation, body)
            .await
    }

    /// Create `bucket` through Amazon S3's own `CreateBucket`.
    async fn create_bucket(&self, case: &str, bucket: &str) {
        let created = self
            .s3(
                "PUT",
                &format!("/{bucket}"),
                &format!(
                    "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
                     <LocationConstraint>{REGION}</LocationConstraint>\
                     </CreateBucketConfiguration>"
                ),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the bucket {bucket} must be created, got {} with {}",
            created.status, created.body
        );
    }

    /// Write `body` at `key` in `bucket`.
    async fn put_object(&self, case: &str, bucket: &str, key: &str, body: &str) {
        let written = self.s3("PUT", &format!("/{bucket}/{key}"), body).await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: {key} must be written, got {} with {}",
            written.status, written.body
        );
    }

    /// The body of one object, or `Err` with the code the store refused the read with.
    async fn object_body(&self, bucket: &str, key: &str) -> Result<String, String> {
        let read = self.s3("GET", &format!("/{bucket}/{key}"), "").await;
        if read.status == 200 {
            return Ok(read.body);
        }
        Err(opt_element(&read.body, "Code").unwrap_or_else(|| format!("HTTP {}", read.status)))
    }

    /// Create `role_name` with `trust_document`, grant it the object access every row needs,
    /// attach the managed policy a client attaches, and answer the ARN IAM minted.
    async fn create_role(
        &self,
        case: &str,
        role_name: &str,
        trust_document: &Value,
        grants: &Value,
    ) -> String {
        let created = self
            .iam(
                case,
                "CreateRole",
                &[
                    ("RoleName", role_name),
                    ("AssumeRolePolicyDocument", &trust_document.to_string()),
                ],
            )
            .await;
        let arn = element(&created, "Arn");
        assert!(
            !arn.is_empty(),
            "case {case:?}: CreateRole must answer the role's ARN, got {created}"
        );
        self.iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", role_name),
                ("PolicyName", "boundary-access"),
                ("PolicyDocument", &grants.to_string()),
            ],
        )
        .await;
        self.iam(
            case,
            "AttachRolePolicy",
            &[
                ("RoleName", role_name),
                ("PolicyArn", GLUE_SERVICE_ROLE_POLICY),
            ],
        )
        .await;
        arn
    }

    /// Create a `pythonshell` job called `job` running the script at `script_key` as `role`.
    async fn create_job(&self, case: &str, job: &str, role: &str, bucket: &str, script_key: &str) {
        self.glue(
            case,
            "CreateJob",
            &json!({
                "Name": job,
                "Role": role,
                "Command": {
                    "Name": "pythonshell",
                    "PythonVersion": "3.9",
                    "ScriptLocation": format!("s3://{bucket}/{script_key}"),
                },
                "MaxCapacity": 0.0625,
                "Timeout": 5,
            }),
        )
        .await;
    }

    /// Start one run of `job` writing `marker_key`, and answer the run's id.
    async fn start_run(&self, case: &str, job: &str, bucket: &str, marker_key: &str) -> String {
        let started = self
            .glue(
                case,
                "StartJobRun",
                &json!({
                    "JobName": job,
                    "Arguments": { "--bucket": bucket, "--marker_key": marker_key },
                }),
            )
            .await;
        started["JobRunId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: StartJobRun must answer a JobRunId, got {started}")
            })
            .to_string()
    }

    /// Poll `GetJobRun` until the run has come to rest, exactly as a client waits for one, and
    /// answer the `JobRun` it then reports.
    async fn run_at_rest(&self, case: &str, job: &str, run_id: &str) -> Value {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let reported = self
                .glue(
                    case,
                    "GetJobRun",
                    &json!({ "JobName": job, "RunId": run_id }),
                )
                .await;
            let run = reported["JobRun"].clone();
            let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
            if TERMINAL_RUN_STATES.contains(&state.as_str()) {
                return run;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the run never came to rest; it reports {run}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }

    /// Create a queue called `name`; answers its url and the ARN a rule names it by.
    async fn queue(&self, case: &str, name: &str) -> (String, String) {
        let created = self
            .sqs(case, "CreateQueue", &json!({"QueueName": name}))
            .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: CreateQueue must answer a url, got {created}")
            })
            .to_string();
        let attributes = self
            .sqs(
                case,
                "GetQueueAttributes",
                &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
            )
            .await;
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the queue must report its ARN, got {attributes}")
            })
            .to_string();
        (url, arn)
    }

    /// Every message now waiting on the queue at `url`, as the envelopes their bodies carry.
    async fn drain(&self, case: &str, url: &str) -> Vec<Value> {
        let received = self
            .sqs(
                case,
                "ReceiveMessage",
                &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
            )
            .await;
        received["Messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|message| {
                serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
                    .unwrap_or(Value::Null)
            })
            .collect()
    }
}

/// A trust document carrying `statements`.
fn trust(statements: Value) -> Value {
    json!({"Version": "2012-10-17", "Statement": statements})
}

/// One trust statement: `effect` the `action` for the service principals `principals`.
fn service_statement(effect: &str, principals: Value, action: Value) -> Value {
    json!({"Effect": effect, "Principal": {"Service": principals}, "Action": action})
}

/// The trust document a role every delivery row shares carries: it admits Glue, because what
/// those rows vary is the rule's pattern and the target's policy.
fn admits_glue() -> Value {
    trust(json!([service_statement(
        "Allow",
        json!(GLUE_PRINCIPAL),
        json!(ASSUME_ROLE)
    )]))
}

/// The object access every row's role is given: the reads and writes the run itself makes, so no
/// row's outcome can follow from an identity policy rather than from the trust document.
fn object_grants(bucket: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["s3:GetObject", "s3:PutObject"],
                "Resource": format!("arn:aws:s3:::{bucket}/*"),
            },
            {
                "Effect": "Allow",
                "Action": "s3:ListBucket",
                "Resource": format!("arn:aws:s3:::{bucket}"),
            },
        ],
    })
}

/// Run `body` on a thread sized for the router assembly, and carry a row's failure out of it.
fn on_the_emulator<Body>(name: &'static str, body: Body)
where
    Body: FnOnce(&Emulator, &tokio::runtime::Runtime) + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let emulator = Emulator::start();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(4)
                .build()
                .expect("a multi-thread runtime");
            body(&emulator, &runtime);
        })
        .expect("spawn the suite's thread")
        .join()
        .unwrap_or_else(|_| panic!("{name}"));
}

/// One row of the trust table: the document the run's role carries, and what the run must come
/// to.
struct RunRow {
    case: &'static str,
    /// The label the row's role, job, script and marker are named after.
    label: &'static str,
    /// The role's `AssumeRolePolicyDocument`.
    trust: Value,
    /// Whether the document admits the Glue service principal the assume, which is the only
    /// thing a row varies.
    admits_glue: bool,
}

/// The rows: the principal admitted, and the forms a trust document names one in.
fn run_rows() -> Vec<RunRow> {
    vec![
        RunRow {
            case: "a document admitting Glue",
            label: "glue-trusted",
            trust: admits_glue(),
            admits_glue: true,
        },
        RunRow {
            case: "a document admitting Glue among several principals",
            label: "glue-among-several",
            trust: trust(json!([service_statement(
                "Allow",
                json!(["ec2.amazonaws.com", GLUE_PRINCIPAL]),
                json!(["sts:TagSession", ASSUME_ROLE])
            )])),
            admits_glue: true,
        },
        RunRow {
            case: "a document admitting another service only",
            label: "ec2-trusted",
            trust: trust(json!([service_statement(
                "Allow",
                json!("ec2.amazonaws.com"),
                json!(ASSUME_ROLE)
            )])),
            admits_glue: false,
        },
        RunRow {
            case: "a document admitting Glue for another action only",
            label: "glue-other-action",
            trust: trust(json!([service_statement(
                "Allow",
                json!(GLUE_PRINCIPAL),
                json!("sts:TagSession")
            )])),
            admits_glue: false,
        },
    ]
}

/// RULE 1: a job run is performed as the job's `Role`, so a role whose trust document admits the
/// Glue service principal runs to `SUCCEEDED` leaving its script's object behind, and one whose
/// document withholds that assume comes to rest `FAILED` stating the requirement and naming the
/// role, with nothing written.
#[test]
fn a_job_run_settles_by_the_trust_document_its_role_carries() {
    on_the_emulator("xsvc-boundary-job-run", |emulator, runtime| {
        runtime.block_on(async {
            let bucket = "xsvc-boundary-runs";
            emulator.create_bucket("setup", bucket).await;

            for row in run_rows() {
                let case = row.case;
                let role = emulator
                    .create_role(
                        case,
                        &format!("xsvc-boundary-{}", row.label),
                        &row.trust,
                        &object_grants(bucket),
                    )
                    .await;
                let script_key = format!("scripts/{}.py", row.label);
                emulator
                    .put_object(case, bucket, &script_key, WRITES_ITS_MARKER)
                    .await;
                let job = format!("xsvc_boundary_{}", row.label.replace('-', "_"));
                emulator
                    .create_job(case, &job, &role, bucket, &script_key)
                    .await;

                let marker_key = format!("markers/{}.txt", row.label);
                let run_id = emulator.start_run(case, &job, bucket, &marker_key).await;
                let run = emulator.run_at_rest(case, &job, &run_id).await;
                let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
                let message = run["ErrorMessage"].as_str().unwrap_or_default().to_string();
                let marker = emulator.object_body(bucket, &marker_key).await;

                if row.admits_glue {
                    assert_eq!(
                        state, "SUCCEEDED",
                        "case {case:?}: expected SUCCEEDED, got {run}"
                    );
                    assert_eq!(
                        marker.as_deref(),
                        Ok(MARKER_BODY),
                        "case {case:?}: expected the script's marker object, got {marker:?}"
                    );
                    continue;
                }
                assert_eq!(
                    state, "FAILED",
                    "case {case:?}: expected FAILED, got {run}"
                );
                // The reason states the requirement as Glue words it for this class, with the
                // role the job named as its one variable part.
                let required = assume_role_requirement(&role);
                assert!(
                    message.contains(&required),
                    "case {case:?}: expected an ErrorMessage carrying {required:?}, got \
                     {message:?}"
                );
                // The script was never executed, so the key it would have written is one the
                // object store does not hold: "NoSuchKey — The specified key does not exist."
                assert_eq!(
                    marker,
                    Err("NoSuchKey".to_string()),
                    "case {case:?}: a run that was never dispatched writes nothing, got \
                     {marker:?}"
                );
            }
        });
    });
}

/// One row of the delivery table: the dimension the rule's pattern filters the envelope on.
struct DeliveryRow {
    case: &'static str,
    label: &'static str,
    /// The `detail` clause the pattern carries beyond the source and the detail type, built over
    /// the job's own name. `None` filters on the source and detail type alone.
    detail: Option<fn(&str) -> Value>,
}

/// RULE 2: a run that comes to rest is published onto the default bus, and each rule whose
/// pattern matches the envelope hands it to every target whose own policy admits the delivery —
/// and to no target that granted it nothing.
#[test]
fn a_settled_run_reaches_only_the_queue_whose_policy_admits_eventbridge() {
    on_the_emulator("xsvc-boundary-delivery", |emulator, runtime| {
        runtime.block_on(async {
            let bucket = "xsvc-boundary-delivery";
            emulator.create_bucket("setup", bucket).await;
            let role = emulator
                .create_role(
                    "setup",
                    "xsvc-boundary-delivery-role",
                    &admits_glue(),
                    &object_grants(bucket),
                )
                .await;

            let rows = [
                DeliveryRow {
                    case: "the job name and the state inside the detail",
                    label: "named",
                    detail: Some(|job| json!({"jobName": [job], "state": ["SUCCEEDED"]})),
                },
                DeliveryRow {
                    case: "the source and the detail type alone",
                    label: "broad",
                    detail: None,
                },
            ];

            for row in rows {
                let case = row.case;
                let script_key = format!("scripts/{}.py", row.label);
                emulator
                    .put_object(case, bucket, &script_key, WRITES_ITS_MARKER)
                    .await;
                let job = format!("xsvc_delivery_{}", row.label);
                emulator
                    .create_job(case, &job, &role, bucket, &script_key)
                    .await;

                // The two targets of ONE rule: they differ in their own resource policy and in
                // nothing else.
                let (permitted_url, permitted_arn) = emulator
                    .queue(case, &format!("xsvc-delivery-permitted-{}", row.label))
                    .await;
                let (withheld_url, withheld_arn) = emulator
                    .queue(case, &format!("xsvc-delivery-withheld-{}", row.label))
                    .await;

                let mut pattern = json!({
                    "source": [GLUE_EVENT_SOURCE],
                    "detail-type": [JOB_STATE_CHANGE],
                });
                if let Some(clause) = row.detail {
                    pattern["detail"] = clause(&job);
                }
                let rule_name = format!("xsvc-delivery-rule-{}", row.label);
                let put = emulator
                    .events(
                        case,
                        "PutRule",
                        &json!({
                            "Name": &rule_name,
                            "EventBusName": DEFAULT_BUS,
                            "EventPattern": pattern.to_string(),
                            "State": "ENABLED",
                        }),
                    )
                    .await;
                let rule_arn = put["RuleArn"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: PutRule must answer the rule's ARN, got {put}")
                    })
                    .to_string();
                let targeted = emulator
                    .events(
                        case,
                        "PutTargets",
                        &json!({
                            "Rule": &rule_name,
                            "EventBusName": DEFAULT_BUS,
                            "Targets": [
                                {"Id": "permitted", "Arn": &permitted_arn},
                                {"Id": "withheld", "Arn": &withheld_arn},
                            ],
                        }),
                    )
                    .await;
                assert_eq!(
                    targeted["FailedEntryCount"],
                    json!(0),
                    "case {case:?}: both targets must be accepted, got {targeted}"
                );
                // Only ONE of the two queues grants the delivery: `sqs:SendMessage` to the
                // EventBridge service principal, scoped to the rule that delivers.
                let policy = json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Sid": "AllowEventBridge",
                        "Effect": "Allow",
                        "Principal": {"Service": EVENTS_PRINCIPAL},
                        "Action": "sqs:SendMessage",
                        "Resource": &permitted_arn,
                        "Condition": {"ArnEquals": {"aws:SourceArn": &rule_arn}},
                    }],
                })
                .to_string();
                emulator
                    .sqs(
                        case,
                        "SetQueueAttributes",
                        &json!({"QueueUrl": &permitted_url, "Attributes": {"Policy": policy}}),
                    )
                    .await;

                let marker_key = format!("markers/{}.txt", row.label);
                let run_id = emulator.start_run(case, &job, bucket, &marker_key).await;
                let run = emulator.run_at_rest(case, &job, &run_id).await;
                let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
                assert_eq!(
                    state, "SUCCEEDED",
                    "case {case:?}: the run this delivery is about must succeed, got {run}"
                );

                // The permitted queue receives the envelope with NO further Glue call: the
                // publication is the service's own, not a side effect of being read.
                let deadline = Instant::now() + DELIVERY_DEADLINE;
                let mut arrived: Vec<Value> = Vec::new();
                while arrived.is_empty() {
                    arrived.extend(emulator.drain(case, &permitted_url).await);
                    if !arrived.is_empty() {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "case {case:?}: the permitted queue received no delivery of the run's \
                         state change"
                    );
                    tokio::time::sleep(LOOK_INTERVAL).await;
                }
                let envelope = arrived[0].clone();
                for (pointer, expected) in [
                    ("/source", json!(GLUE_EVENT_SOURCE)),
                    ("/detail-type", json!(JOB_STATE_CHANGE)),
                    ("/account", json!(ACCOUNT)),
                    ("/region", json!(REGION)),
                    ("/detail/jobName", json!(&job)),
                    ("/detail/state", json!("SUCCEEDED")),
                    ("/detail/jobRunId", json!(&run_id)),
                ] {
                    let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
                    assert_eq!(
                        actual, expected,
                        "case {case:?}: expected {pointer} to be {expected}, got {actual} in \
                         {envelope}"
                    );
                }

                // The sibling target granted the delivery nothing, so it holds nothing — read
                // only AFTER the permitted queue had its own copy, and after a further span of
                // the clock.
                tokio::time::sleep(QUIET_SPAN).await;
                let withheld = emulator.drain(case, &withheld_url).await;
                assert!(
                    withheld.is_empty(),
                    "case {case:?}: a queue whose policy admits EventBridge nothing must \
                     receive nothing, got {withheld:?}"
                );
            }
        });
    });
}
