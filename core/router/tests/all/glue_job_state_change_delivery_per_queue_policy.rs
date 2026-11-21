// SPDX-License-Identifier: CC0-1.0

//! A Glue job run's terminal `Glue Job State Change` event reaches each SQS target of its
//! EventBridge rule exactly when THAT queue's own resource policy admits the delivery, on a
//! router that VERIFIES request signatures — the configuration the emulator serves traffic in.
//!
//! The RULE under test: **EventBridge invokes an SQS target as its own service principal
//! (`events.amazonaws.com`) and "relies on resource-based policies" (`PutTargets`), so the
//! queue's own `Policy` alone decides that delivery, per target and independently of every
//! other target of the same rule — a queue whose policy allows that principal
//! `sqs:SendMessage` for the invoking rule receives the event, and one that allows it nothing,
//! denies it, or allows it only for conditions this invocation does not satisfy receives
//! nothing, while the rest of the rule's targets are unaffected.** The condition keys such a
//! grant is scoped by are the ones a cross-service call carries: `aws:SourceArn`, "the ARN of
//! the rule", and `aws:SourceAccount`, the account that rule belongs to.
//!
//! The rows are the POLICY FORMS a queue target carries, which is what the rule generalises
//! over: an unconditioned grant, grants scoped by each of those two condition keys (holding
//! and not holding), no policy at all, and an explicit deny beside a grant. The recorded case
//! — one permissioned queue and one sibling queue with no policy at all, under a rule matching
//! `source` `aws.glue` / `detail-type` `Glue Job State Change` / `detail.state` `SUCCEEDED` —
//! is two of these rows; all of them are targets of rules matching that one event and are
//! judged over ONE job run, so a delivery that reached the wrong queue is visible as such.
//!
//! What only this suite reaches: the whole assembled emulator with request authentication ON,
//! where the job's role is IAM's own, the script is fetched from Amazon S3, the run's
//! publication travels the emulator's own callback into EventBridge, and the deliveries are
//! real `SendMessage` calls on real queues — every one of them signature-verified. The Glue
//! crate's `job_run_state_change_events.rs` pins the publication itself over four terminal
//! states with authentication off and a hand-written role ARN.
//!
//! Nothing here waits on the clock: the run is polled until it has come to rest, the delivery
//! until it has arrived, each against a generous deadline, and no assertion reads elapsed time.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on an explicitly sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for and addressed in.
const REGION: &str = "us-west-2";

/// The XML namespace Amazon S3's own model declares, which a client's payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The `source` and `detail-type` a Glue job run's terminal state is published under, spelled
/// out here rather than imported, so a wrong constant in the implementation cannot make this
/// suite pass.
const SOURCE: &str = "aws.glue";
const DETAIL_TYPE: &str = "Glue Job State Change";

/// The service principal EventBridge delivers to an SQS target as, which the queue's own
/// policy has to allow the send for.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The action such a delivery performs on the queue.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The bucket the job's script lives in, and the key it is staged under.
const BUCKET: &str = "xsvc-glue-job-state-change";
const SCRIPT_KEY: &str = "scripts/job.py";

/// The script the job runs: it exits cleanly, which is what a succeeding job does.
const SCRIPT: &str = "print(\"ran\")\n";

/// The trust document a Glue job's role carries: Glue assumes it to perform the run. It
/// carries none of the characters that are significant in a form body, so it travels as
/// written once encoded.
const GLUE_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"glue.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The grants the run's role needs to read its script: the object read, and the bucket listing
/// that decides WHICH refusal a missing object earns.
const SCRIPT_ACCESS: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Action":["s3:GetObject","s3:ListBucket"],"Resource":["#,
    r#""arn:aws:s3:::xsvc-glue-job-state-change","#,
    r#""arn:aws:s3:::xsvc-glue-job-state-change/*"]}]}"#
);

/// The policy AWS publishes for a Glue job's role, which the recorded case attaches beside the
/// inline grants above.
const GLUE_SERVICE_ROLE_POLICY: &str = "arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole";

/// The `JobRunState` a run that executed its script cleanly comes to rest in, and every state
/// a run never leaves again
/// (`smithy_query(query="shape",shape="JobRunState",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`).
const SUCCEEDED: &str = "SUCCEEDED";
const TERMINAL_RUN_STATES: [&str; 6] = [
    "STOPPED",
    "SUCCEEDED",
    "FAILED",
    "TIMEOUT",
    "ERROR",
    "EXPIRED",
];

/// How long the run is waited for, how long a delivery is waited for, and how often each is
/// looked at. Generous: what is asserted is that each one happens at all, never how fast the
/// host runs.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const DELIVERY_DEADLINE: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long the withheld queues keep being looked at after every admitted delivery has
/// arrived, before concluding that nothing is coming. A bound on an ABSENCE, so it is the one
/// span a slow host may spend without changing the verdict.
const WITHHELD_WINDOW: Duration = Duration::from_secs(5);

/// How many targets one rule carries: "each rule can have up to five (5) targets associated
/// with it at one time" (`PutTargets`), so the rows are spread over as many rules as that
/// takes.
const TARGETS_PER_RULE: usize = 5;

/// The key pair this process publishes once request authentication is on, which every call
/// below is signed with.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization` header an SDK
    /// request carries.
    fn sign(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// What one call answered.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// The body as JSON, or `Value::Null` for one that is not.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator this suite drives: the whole router over one engine, serving as it serves
/// traffic — with request authentication ON.
struct Emulator {
    router: MultiServiceRouter,
    signer: Signer,
}

impl Emulator {
    fn start() -> Self {
        let engine = aws_emulator_router::new_engine();
        let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
        router.set_request_authentication(true);
        let published = ::emulator_server::SigV4Authenticator::minted_credentials()
            .expect("turning authentication on publishes this process's keys");
        Self {
            signer: Signer {
                key_id: published.access_key_id.clone(),
                secret: published.secret_access_key.clone(),
            },
            router,
        }
    }

    /// Dispatch one signed request as it arrives on the wire.
    async fn send(
        &self,
        method: &str,
        host: &str,
        signing: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Answer {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let built = builder
            .body(body.as_bytes().to_vec())
            .expect("a well-formed request");
        let (parts, payload) = self.signer.sign(built, signing).into_parts();
        let response = self
            .router
            .dispatch(http::Request::from_parts(
                parts,
                bytes::Bytes::from(payload),
            ))
            .await;
        Answer {
            status: response.status().as_u16(),
            body: String::from_utf8_lossy(response.body()).into_owned(),
        }
    }

    /// One Amazon S3 REST call, path-style as the emulator serves it, which must answer 200.
    async fn s3(&self, what: &str, method: &str, path: &str, body: &str) -> Answer {
        let host = format!("s3.{REGION}.amazonaws.com");
        let answered = self.send(method, &host, "s3", path, &[], body).await;
        assert_eq!(
            answered.status, 200,
            "setup: {what} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered
    }

    /// One IAM `awsQuery` call, which must answer 200.
    async fn iam(&self, what: &str, action: &str, params: &[(&str, &str)]) -> Answer {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
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
            "setup: {what} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered
    }

    /// One `X-Amz-Target` RPC call, which must answer 200.
    async fn rpc(
        &self,
        what: &str,
        signing: &str,
        content_type: &str,
        target: &str,
        body: &Value,
    ) -> Value {
        let host = format!("{signing}.{REGION}.amazonaws.com");
        let answered = self
            .send(
                "POST",
                &host,
                signing,
                "/",
                &[
                    ("content-type", content_type),
                    ("x-amz-target", target),
                ],
                &body.to_string(),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "{what}: {target} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered.json()
    }

    /// One AWS Glue `awsJson1_1` call.
    async fn glue(&self, what: &str, operation: &str, body: &Value) -> Value {
        self.rpc(
            what,
            "glue",
            "application/x-amz-json-1.1",
            &format!("AWSGlue.{operation}"),
            body,
        )
        .await
    }

    /// One EventBridge `awsJson1_1` call.
    async fn events(&self, what: &str, operation: &str, body: &Value) -> Value {
        self.rpc(
            what,
            "events",
            "application/x-amz-json-1.1",
            &format!("AWSEvents.{operation}"),
            body,
        )
        .await
    }

    /// One Amazon SQS `awsJson1_0` call.
    async fn sqs(&self, what: &str, operation: &str, body: &Value) -> Value {
        self.rpc(
            what,
            "sqs",
            "application/x-amz-json-1.0",
            &format!("AmazonSQS.{operation}"),
            body,
        )
        .await
    }

    /// A queue called `name`: its url and the ARN a rule names it by.
    async fn given_queue(&self, case: &str, name: &str) -> (String, String) {
        let created = self
            .sqs(case, "CreateQueue", &json!({ "QueueName": name }))
            .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
            .to_string();
        let attributes = self
            .sqs(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
            .to_string();
        (url, arn)
    }

    /// Install `policy` on the queue at `url` as its `Policy` attribute.
    async fn set_policy(&self, case: &str, url: &str, policy: &Value) {
        self.sqs(
            case,
            "SetQueueAttributes",
            &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
        )
        .await;
    }

    /// Receive everything waiting on the queue at `url` and delete it; answers the parsed
    /// envelopes its message bodies carried.
    ///
    /// Each message received is DELETED, so a message this answered with cannot come back when
    /// its visibility timeout lapses and be read as a second delivery of the same event.
    async fn drain(&self, case: &str, url: &str) -> Vec<Value> {
        let received = self
            .sqs(
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        let mut envelopes = Vec::with_capacity(messages.len());
        for message in &messages {
            if let Some(handle) = message["ReceiptHandle"].as_str() {
                self.sqs(
                    case,
                    "DeleteMessage",
                    &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
                )
                .await;
            }
            envelopes.push(
                serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
                    .unwrap_or(Value::Null),
            );
        }
        envelopes
    }

    /// Poll `GetJobRun` until the run comes to rest, exactly as a client waits for one, and
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
}

/// One statement of a queue's policy.
fn statement(effect: &str, resource: &str, condition: Option<Value>) -> Value {
    let mut built = json!({
        "Effect": effect,
        "Principal": { "Service": EVENTS_PRINCIPAL },
        "Action": SEND_MESSAGE,
        "Resource": resource,
    });
    if let Some(condition) = condition {
        built["Condition"] = condition;
    }
    built
}

/// A whole policy document over `statements`.
fn policy(statements: Vec<Value>) -> Value {
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// One row: the policy form one target queue carries, and whether the delivery happens.
struct Row {
    /// The case name every assertion of the row reports, which is also its queue's name.
    case: &'static str,
    /// The queue's own `Policy`, built from the ARN of the queue and the ARN of the invoking
    /// rule — or `None` for a queue that carries no resource policy at all.
    policy: fn(&str, &str) -> Option<Value>,
    /// Whether the event must reach this queue.
    delivered: bool,
}

/// The rows: every policy form a queue target carries, spanning what the rule turns on.
///
/// The ARN of a rule of this account that is NOT the invoking one is built by naming another
/// rule in the invoking rule's own ARN, so the row differs from the one above it in the one
/// value its condition compares.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an unconditioned grant to the events principal",
            policy: |queue_arn, _| Some(policy(vec![statement("Allow", queue_arn, None)])),
            delivered: true,
        },
        Row {
            case: "a grant scoped by ArnEquals to the invoking rule",
            policy: |queue_arn, rule_arn| {
                Some(policy(vec![statement(
                    "Allow",
                    queue_arn,
                    Some(json!({ "ArnEquals": { "aws:SourceArn": rule_arn } })),
                )]))
            },
            delivered: true,
        },
        Row {
            case: "a grant scoped by ArnLike to every rule of this account",
            policy: |queue_arn, rule_arn| {
                let every_rule = rule_arn
                    .rsplit_once(':')
                    .map_or_else(String::new, |(account, _)| format!("{account}:rule/*"));
                Some(policy(vec![statement(
                    "Allow",
                    queue_arn,
                    Some(json!({ "ArnLike": { "aws:SourceArn": every_rule } })),
                )]))
            },
            delivered: true,
        },
        Row {
            case: "a grant scoped by StringEquals to the account the rule belongs to",
            policy: |queue_arn, _| {
                Some(policy(vec![statement(
                    "Allow",
                    queue_arn,
                    Some(json!({
                        "StringEquals": { "aws:SourceAccount": engine::EMULATOR_ACCOUNT_ID },
                    })),
                )]))
            },
            delivered: true,
        },
        Row {
            case: "no resource policy at all",
            policy: |_, _| None,
            delivered: false,
        },
        Row {
            case: "a grant scoped by ArnEquals to another rule",
            policy: |queue_arn, rule_arn| {
                let another = format!("{rule_arn}-elsewhere");
                Some(policy(vec![statement(
                    "Allow",
                    queue_arn,
                    Some(json!({ "ArnEquals": { "aws:SourceArn": another } })),
                )]))
            },
            delivered: false,
        },
        Row {
            case: "an explicit deny beside an unconditioned grant",
            policy: |queue_arn, _| {
                Some(policy(vec![
                    statement("Allow", queue_arn, None),
                    statement("Deny", queue_arn, None),
                ]))
            },
            delivered: false,
        },
        Row {
            case: "a grant of another action only",
            policy: |queue_arn, _| {
                Some(policy(vec![json!({
                    "Effect": "Allow",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": "sqs:ReceiveMessage",
                    "Resource": queue_arn,
                })]))
            },
            delivered: false,
        },
    ]
}

/// RULE: the `Glue Job State Change` event a job run publishes when it comes to rest reaches
/// each SQS target of its rule exactly when that queue's own resource policy admits
/// `events.amazonaws.com` the send for the invoking rule, and the verdict for one target does
/// not change the verdict for any other.
#[test]
fn a_queue_targets_own_policy_decides_whether_it_receives_the_job_state_change() {
    std::thread::Builder::new()
        .name("glue-job-state-change-delivery".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .build()
                .expect("a multi-thread runtime");
            runtime.block_on(drive());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// Every row, as a target of a rule matching one job run's publication.
async fn drive() {
    let emulator = Emulator::start();
    let rows = rows();

    // The script the run executes, staged through Amazon S3's own operations.
    emulator
        .s3(
            "the bucket",
            "PUT",
            &format!("/{BUCKET}"),
            &format!(
                "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
                 <LocationConstraint>{REGION}</LocationConstraint>\
                 </CreateBucketConfiguration>"
            ),
        )
        .await;
    emulator
        .s3(
            "the script",
            "PUT",
            &format!("/{BUCKET}/{SCRIPT_KEY}"),
            SCRIPT,
        )
        .await;

    // The role the run is performed as, created and granted through IAM's own operations.
    let role_name = "glue-job-state-change-role";
    let created = emulator
        .iam(
            "the role",
            "CreateRole",
            &[
                ("RoleName", role_name),
                ("AssumeRolePolicyDocument", GLUE_TRUST),
            ],
        )
        .await;
    let role_arn = element(&created.body, "Arn");
    assert!(
        !role_arn.is_empty(),
        "setup: CreateRole must answer the role's ARN, got {}",
        created.body
    );
    emulator
        .iam(
            "the role's grants",
            "PutRolePolicy",
            &[
                ("RoleName", role_name),
                ("PolicyName", "read-the-script"),
                ("PolicyDocument", SCRIPT_ACCESS),
            ],
        )
        .await;
    // The managed policy AWS publishes for a Glue job's role, attached as the recorded case
    // attaches it: the grants in force on the role are its inline ones AND its attachments.
    emulator
        .iam(
            "the role's attachment",
            "AttachRolePolicy",
            &[("RoleName", role_name), ("PolicyArn", GLUE_SERVICE_ROLE_POLICY)],
        )
        .await;

    // The queues, one per row, and the rules that target them. "A rule can have up to five
    // (5) targets associated with it at one time" (`PutTargets`), so the rows are spread over
    // as many rules as that takes, each matching the same event — and each row's policy is
    // written against the ARN of the rule whose target it is, so a grant scoped to the
    // invoking rule is scoped to the one that invokes it.
    let mut queues: Vec<(String, String)> = Vec::with_capacity(rows.len());
    for row in &rows {
        queues.push(
            emulator
                .given_queue(row.case, row.case.replace(' ', "-").as_str())
                .await,
        );
    }

    let job_name = "glue_job_state_change";
    for (group, indices) in (0..rows.len())
        .collect::<Vec<usize>>()
        .chunks(TARGETS_PER_RULE)
        .enumerate()
    {
        let rule_name = format!("glue-job-state-change-rule-{group}");
        let put = emulator
            .events(
                "the rule",
                "PutRule",
                &json!({
                    "Name": &rule_name,
                    "State": "ENABLED",
                    "EventPattern": json!({
                        "source": [SOURCE],
                        "detail-type": [DETAIL_TYPE],
                        "detail": { "jobName": [job_name], "state": [SUCCEEDED] },
                    })
                    .to_string(),
                }),
            )
            .await;
        let rule_arn = put["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("setup: PutRule must answer the rule's ARN, got {put}"))
            .to_string();

        for index in indices {
            let (url, queue_arn) = &queues[*index];
            if let Some(document) = (rows[*index].policy)(queue_arn, &rule_arn) {
                emulator.set_policy(rows[*index].case, url, &document).await;
            }
        }
        // The targets go in with the ones whose policy WITHHOLDS the delivery first, and each
        // is named as the recorded case names them: the verdict for one target is the
        // target's own, so a delivery that a preceding refusal stopped would show here.
        let mut ordered: Vec<usize> = indices.to_vec();
        ordered.sort_by_key(|index| rows[*index].delivered);
        let targets: Vec<Value> = ordered
            .iter()
            .map(|index| {
                let id = if rows[*index].delivered { "ok" } else { "broken" };
                json!({ "Id": format!("{id}-{index}"), "Arn": &queues[*index].1 })
            })
            .collect();
        let targeted = emulator
            .events(
                "the targets",
                "PutTargets",
                &json!({ "Rule": &rule_name, "Targets": targets }),
            )
            .await;
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "setup: every queue target must be accepted, got {targeted}"
        );
    }

    // One job, one run: the publication every row is judged over.
    emulator
        .glue(
            "the job",
            "CreateJob",
            &json!({
                "Name": job_name,
                "Role": role_arn,
                "Command": {
                    "Name": "pythonshell",
                    "PythonVersion": "3.9",
                    "ScriptLocation": format!("s3://{BUCKET}/{SCRIPT_KEY}"),
                },
                "MaxCapacity": 0.0625,
                "Timeout": 5,
            }),
        )
        .await;
    let started = emulator
        .glue("the run", "StartJobRun", &json!({ "JobName": job_name }))
        .await;
    let run_id = started["JobRunId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: StartJobRun must answer a JobRunId, got {started}"))
        .to_string();
    let run = emulator.run_at_rest("the run", job_name, &run_id).await;
    assert_eq!(
        run["JobRunState"].as_str(),
        Some(SUCCEEDED),
        "the run of a script that exits cleanly must come to rest in {SUCCEEDED}, got {run}"
    );

    // Each admitted delivery is waited for until it is there; the withheld ones are looked at
    // throughout and for a further window afterwards.
    let mut held: Vec<Vec<Value>> = vec![Vec::new(); rows.len()];
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        for (index, (url, _)) in queues.iter().enumerate() {
            let drained = emulator.drain(rows[index].case, url).await;
            held[index].extend(drained);
        }
        let outstanding: Vec<&str> = rows
            .iter()
            .enumerate()
            .filter(|(index, row)| row.delivered && held[*index].is_empty())
            .map(|(_, row)| row.case)
            .collect();
        if outstanding.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no event reached the queue of {outstanding:?} before the deadline, while the \
             queues hold {held:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    let watching = Instant::now();
    while watching.elapsed() < WITHHELD_WINDOW {
        for (index, (url, _)) in queues.iter().enumerate() {
            let drained = emulator.drain(rows[index].case, url).await;
            held[index].extend(drained);
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let received = &held[index];
        if !row.delivered {
            assert!(
                received.is_empty(),
                "case {case:?}: a queue whose policy does not admit the delivery receives \
                 nothing, got {received:?}"
            );
            continue;
        }
        assert_eq!(
            received.len(),
            1,
            "case {case:?}: expected exactly one delivered event, got {received:?}"
        );
        let envelope = &received[0];
        for (pointer, expected) in [
            ("/source", json!(SOURCE)),
            ("/detail-type", json!(DETAIL_TYPE)),
            ("/region", json!(REGION)),
            ("/account", json!(engine::EMULATOR_ACCOUNT_ID)),
            ("/detail/jobName", json!(job_name)),
            ("/detail/jobRunId", json!(&run_id)),
            ("/detail/state", json!(SUCCEEDED)),
        ] {
            let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {actual} in {envelope}"
            );
        }
    }
}
