// SPDX-License-Identifier: CC0-1.0

//! A job run started against a CloudFormation-DEPLOYED job reaches the queue the SAME stack's
//! EventBridge rule, queues and queue policies wire up.
//!
//! The rule under test: **a job run that comes to rest in a terminal `JobRunState` is published
//! once onto the account's DEFAULT EventBridge bus as `source` `aws.glue` / `detail-type` `Glue
//! Job State Change`, carrying as the event `detail` that run's own stored members — the job it
//! belongs to, its own run id, the state it came to rest in, how severe that state is for the
//! job and, where the run carries one, the reason it failed — and EventBridge routes the
//! envelope by its CONTENT, handing it UNCHANGED to every target of each rule whose
//! `EventPattern` matches, but only where that TARGET's own resource policy admits the
//! delivery: a queue granting `events.amazonaws.com` the send (with no condition, or scoped by
//! `aws:SourceArn` to the delivering rule) receives the envelope as its message body, while a
//! queue that granted that principal nothing — or granted it only for ANOTHER rule — receives
//! nothing and reports `ApproximateNumberOfMessages` 0.** None of that turns on who created the
//! job, the rule or the queues: a pipeline a client PROVISIONED FROM A TEMPLATE observes the
//! run's completion on its own queue with no further Glue call.
//!
//! Sources: `StartJobRun` ("starts a job run using a job definition") and `GetJobRun` ->
//! `JobRun{JobRunState, ErrorMessage}` for what the run reports, and the terminal `JobRunState`
//! values of this crate's own model. EventBridge models no event payload in Smithy, so each
//! `detail` member is asserted against what the SAME run's control-plane read reports for it,
//! never against a recorded value.
//!
//! Six crates have to agree for that to hold — CloudFormation makes every create the template
//! declares, Amazon S3 holds the bucket the script is staged in, IAM holds the role the run is
//! performed as, this service runs the script and publishes the state the run came to rest in,
//! EventBridge matches the envelope and hands it over, and Amazon SQS holds what arrived under
//! its own queue policy — so all six are mounted on ONE engine here, the way
//! `core/router/src/lib.rs` mounts them, and every call travels the real in-process callback.
//!
//! The rows differ along what the rule generalises over: WHICH terminal state the run's own
//! script leads it to (`SUCCEEDED` for a script that exits cleanly, `FAILED` for one that
//! raises), WHICH dimension of the envelope the stack's rule filters on (the source and detail
//! type alone, the job name together with one state inside `detail`, one state alone, a state
//! the run never takes), and WHAT the target queue's policy admits (an unscoped grant, a grant
//! scoped by `aws:SourceArn` to the delivering rule, a grant scoped to another rule of the
//! account). Every row carries a POSITIVE CONTROL rather than a bare quiet window: each stack
//! also declares a second rule matching the source and detail type alone whose target is a
//! control queue with an unscoped grant, so the control's message is what says the envelope
//! really was published and dispatched, and a row's own queue holding nothing by then is a
//! refusal rather than a look taken too early. Every row also declares a SIBLING target queue
//! with no policy at all, which is the permission boundary the issue's negative half is about.
//!
//! What is NOT asserted here, because it is asserted beside it:
//! [`crate::job_run_state_change_events`] pins the published `detail` over every terminal state
//! a run of this emulator reaches, driving the runs through this service's own calls.
//!
//! Nothing here sleeps for a span: each wait is for the CONDITION it is about — the stack's
//! status, the run at rest, the message on the queue — looked at again until it holds or a
//! generous deadline passes, and no assertion reads the wall clock.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{ok, s3_client, ACCOUNT_ID, REGION};

/// The `source` and `detail-type` this service publishes a job run's state change under,
/// spelled out here rather than imported, so a wrong constant in the implementation cannot make
/// this suite pass.
const EVENT_SOURCE: &str = "aws.glue";
const JOB_STATE_CHANGE: &str = "Glue Job State Change";

/// The `JobRunState` values the rows are stated in, and the one no run of these rows takes.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const TIMEOUT: &str = "TIMEOUT";

/// The `detail.severity` of a run that came to rest having failed at nothing, and of one that
/// failed.
const SEVERITY_INFO: &str = "INFO";
const SEVERITY_ERROR: &str = "ERROR";

/// The `JobRunState` values a run never leaves again, which is what "at rest" means while
/// polling (this crate's own `JobRunState` enumeration).
const TERMINAL_RUN_STATES: [&str; 6] = [
    "STOPPED",
    "SUCCEEDED",
    "FAILED",
    "TIMEOUT",
    "ERROR",
    "EXPIRED",
];

/// The service principal a cross-service delivery is made as, which the stack's own queue
/// policies admit: for an Amazon SQS queue named as a rule's target, "EventBridge relies on
/// resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The object key the job's script is staged under, and the target ids the stack's rule
/// declares.
const SCRIPT_KEY: &str = "script.py";
const PERMITTED_TARGET_ID: &str = "PermittedQueueTarget";
const UNPERMITTED_TARGET_ID: &str = "UnpermittedQueueTarget";

/// The status a stack that deployed reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// A script that exits cleanly, which is what a succeeding job run does.
const EXITS_CLEANLY: &str = r#"
rows = [index for index in range(4)]
assert sum(rows) == 6, rows
"#;

/// A script that raises, so the run comes to rest on the failure its own execution reached and
/// the envelope carries that reason.
const RAISES: &str = r#"
raise ValueError("the input partition is malformed")
"#;

/// How long a wait for a condition lasts before it is given up on, and how often the condition
/// is looked at. Every deadline here is generous — what is waited for is the condition, not a
/// span — so a loaded or a fast host reaches the same verdict.
const STACK_DEADLINE: Duration = Duration::from_secs(60);
const RUN_DEADLINE: Duration = Duration::from_secs(120);
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);
const QUIET_WINDOW: Duration = Duration::from_secs(3);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// An emulator with this service, CloudFormation, IAM, Amazon S3, EventBridge and Amazon SQS on
/// ONE shared engine.
///
/// Each is mounted because the pipeline really reaches it: the stack makes every create through
/// the service that owns the resource, the run fetches its script from the object store as its
/// own role, and the publication goes onto the default bus where a rule delivers it to a queue.
fn emulator_with_a_stack() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = crate::wire::emulator_on(Arc::clone(&engine));
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// The CloudFormation client this suite deploys through, over the emulator's own in-process
/// callback — the transport every emulated service reaches another through, so nothing outside
/// this process is addressed.
fn cloudformation_client(router: &MultiServiceRouter) -> aws_sdk_cloudformation::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = aws_sdk_cloudformation::Config::builder()
        .behavior_version(aws_sdk_cloudformation::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(aws_sdk_cloudformation::config::Region::new(REGION))
        .endpoint_url(format!("https://cloudformation.{REGION}.amazonaws.com"))
        .retry_config(aws_sdk_cloudformation::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_cloudformation::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_cloudformation::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            aws_sdk_cloudformation::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    aws_sdk_cloudformation::Client::from_conf(config)
}

/// The Amazon SQS client a client reads the delivered envelope with, over the same transport.
fn sqs_client(router: &MultiServiceRouter) -> aws_sdk_sqs::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = aws_sdk_sqs::Config::builder()
        .behavior_version(aws_sdk_sqs::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(aws_sdk_sqs::config::Region::new(REGION))
        .endpoint_url(format!("https://sqs.{REGION}.amazonaws.com"))
        .retry_config(aws_sdk_sqs::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_sqs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_sqs::config::IdentityCache::no_cache())
        .stalled_stream_protection(aws_sdk_sqs::config::StalledStreamProtectionConfig::disabled())
        .build();
    aws_sdk_sqs::Client::from_conf(config)
}

/// The EventBridge client the installed targets are read back through: the same construction
/// every emulated service publishes through.
fn events_client(router: &MultiServiceRouter) -> aws_sdk_eventbridge::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    service_common::eventbridge_client::eventbridge_client(&callback, REGION)
}

/// The ARN this emulator mints for a resource of `service` named `name`, built from the one
/// account id it serves — what the stack's own declarations resolve to.
fn arn(service: &str, name: &str) -> String {
    format!("arn:aws:{service}:{REGION}:{ACCOUNT_ID}:{name}")
}

/// What the target queue's own policy admits.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// The grant carries no `Condition`: any rule of the account may deliver.
    Unscoped,
    /// The grant is scoped by `aws:SourceArn` to the rule that delivers this envelope.
    ThisRule,
    /// The same grant, scoped to ANOTHER rule of the account, which admits nothing of this one.
    AnotherRule,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The script the row's job runs, which is what decides the state its run comes to rest in.
    script: &'static str,
    /// Whether the rule's pattern names the job inside `detail`.
    names_job: bool,
    /// The states the pattern admits inside `detail`; empty filters on the source and detail
    /// type alone.
    states: &'static [&'static str],
    /// What the permissioned queue's policy admits.
    scope: Scope,
    /// The terminal `JobRunState` the run reaches.
    run_state: &'static str,
    /// Whether the row's own queue must receive the envelope.
    delivered: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a succeeding run, a pattern naming the job and SUCCEEDED, and a grant scoped \
                   to the rule",
            script: EXITS_CLEANLY,
            names_job: true,
            states: &[SUCCEEDED],
            scope: Scope::ThisRule,
            run_state: SUCCEEDED,
            delivered: true,
        },
        Row {
            case: "a run whose script raises, on a pattern naming the failure state alone",
            script: RAISES,
            names_job: false,
            states: &[FAILED],
            scope: Scope::Unscoped,
            run_state: FAILED,
            delivered: true,
        },
        Row {
            case: "a pattern matching the source and detail type alone",
            script: EXITS_CLEANLY,
            names_job: false,
            states: &[],
            scope: Scope::Unscoped,
            run_state: SUCCEEDED,
            delivered: true,
        },
        Row {
            case: "a pattern naming a state this run never takes",
            script: EXITS_CLEANLY,
            names_job: true,
            states: &[TIMEOUT],
            scope: Scope::ThisRule,
            run_state: SUCCEEDED,
            delivered: false,
        },
        Row {
            case: "a queue policy scoped to another rule of the account",
            script: EXITS_CLEANLY,
            names_job: true,
            states: &[SUCCEEDED],
            scope: Scope::AnotherRule,
            run_state: SUCCEEDED,
            delivered: false,
        },
    ]
}

/// Every name one row's template declares, so what is asserted is computed from the declaration
/// rather than read back out of the answer being checked.
struct Names {
    stack: String,
    bucket: String,
    role: String,
    job: String,
    queue: String,
    unpermitted_queue: String,
    control_queue: String,
    rule: String,
    control_rule: String,
}

impl Names {
    fn of(index: usize) -> Self {
        Self {
            stack: format!("cfn-glue-events-{index}"),
            bucket: format!("cfn-glue-events-scripts-{index}"),
            role: format!("cfn-glue-events-role-{index}"),
            job: format!("cfn-glue-events-job-{index}"),
            queue: format!("cfn-glue-events-queue-{index}"),
            unpermitted_queue: format!("cfn-glue-events-unpermitted-{index}"),
            control_queue: format!("cfn-glue-events-control-{index}"),
            rule: format!("cfn-glue-events-rule-{index}"),
            control_rule: format!("cfn-glue-events-control-rule-{index}"),
        }
    }

    /// Where the job's script is staged, as `JobCommand$ScriptLocation` names it.
    fn script_location(&self) -> String {
        format!("s3://{}/{SCRIPT_KEY}", self.bucket)
    }

    /// The rule ARN a grant scoped to the DELIVERING rule names, and the one a grant scoped to
    /// another rule of the same account names.
    fn rule_arn(&self) -> String {
        arn("events", &format!("rule/{}", self.rule))
    }
    fn another_rule_arn(&self) -> String {
        arn("events", &format!("rule/{}-elsewhere", self.rule))
    }
}

/// An `Allow` of the send to the service principal on `queue`, under `condition`.
fn queue_policy(queue: Value, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "Service": EVENTS_PRINCIPAL },
        "Action": "sqs:SendMessage",
        "Resource": queue,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// The whole pipeline the issue describes, in ONE template: the bucket the script is staged in,
/// the role the run is performed as, the job itself, the two queues the rule targets with the
/// policy only one of them carries, and the rule — plus the control rule and queue every row is
/// read against.
fn template(row: &Row, names: &Names) -> String {
    let mut pattern = json!({ "source": [EVENT_SOURCE], "detail-type": [JOB_STATE_CHANGE] });
    let mut detail = serde_json::Map::new();
    if row.names_job {
        detail.insert("jobName".to_string(), json!([&names.job]));
    }
    if !row.states.is_empty() {
        detail.insert("state".to_string(), json!(row.states));
    }
    if !detail.is_empty() {
        pattern["detail"] = Value::Object(detail);
    }
    let condition = match row.scope {
        Scope::Unscoped => None,
        Scope::ThisRule => Some(json!({
            "ArnEquals": { "aws:SourceArn": { "Fn::GetAtt": ["Rule", "Arn"] } },
        })),
        Scope::AnotherRule => Some(json!({
            "ArnEquals": { "aws:SourceArn": names.another_rule_arn() },
        })),
    };
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a job whose run's state change fans out to a queue",
        "Resources": {
            "Scripts": {
                "Type": "AWS::S3::Bucket",
                "Properties": { "BucketName": &names.bucket },
            },
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": &names.role,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "glue.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    // The run fetches its own script as this role, so the read it makes is
                    // judged against what the role is granted here.
                    "Policies": [{
                        "PolicyName": "read-the-script",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["s3:GetObject", "s3:ListBucket"],
                                "Resource": [
                                    format!("arn:aws:s3:::{}", names.bucket),
                                    format!("arn:aws:s3:::{}/*", names.bucket),
                                ],
                            }],
                        },
                    }],
                },
            },
            "Job": {
                "Type": "AWS::Glue::Job",
                "DependsOn": ["Scripts"],
                "Properties": {
                    "Name": &names.job,
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Command": {
                        "Name": "pythonshell",
                        "PythonVersion": "3.9",
                        "ScriptLocation": names.script_location(),
                    },
                },
            },
            "Queue": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": &names.queue },
            },
            "QueuePolicy": {
                "Type": "AWS::SQS::QueuePolicy",
                "Properties": {
                    "Queues": [{ "Ref": "Queue" }],
                    "PolicyDocument": queue_policy(
                        json!({ "Fn::GetAtt": ["Queue", "Arn"] }),
                        condition,
                    ),
                },
            },
            // The sibling target with NO policy at all: the permission boundary this emulator
            // has to observe, whatever the rule matches.
            "UnpermittedQueue": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": &names.unpermitted_queue },
            },
            "Rule": {
                "Type": "AWS::Events::Rule",
                "Properties": {
                    "Name": &names.rule,
                    "State": "ENABLED",
                    "EventPattern": pattern,
                    "Targets": [
                        { "Id": PERMITTED_TARGET_ID, "Arn": { "Fn::GetAtt": ["Queue", "Arn"] } },
                        {
                            "Id": UNPERMITTED_TARGET_ID,
                            "Arn": { "Fn::GetAtt": ["UnpermittedQueue", "Arn"] },
                        },
                    ],
                },
            },
            "ControlQueue": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": &names.control_queue },
            },
            "ControlQueuePolicy": {
                "Type": "AWS::SQS::QueuePolicy",
                "Properties": {
                    "Queues": [{ "Ref": "ControlQueue" }],
                    "PolicyDocument": queue_policy(
                        json!({ "Fn::GetAtt": ["ControlQueue", "Arn"] }),
                        None,
                    ),
                },
            },
            "ControlRule": {
                "Type": "AWS::Events::Rule",
                "Properties": {
                    "Name": &names.control_rule,
                    "State": "ENABLED",
                    "EventPattern": {
                        "source": [EVENT_SOURCE],
                        "detail-type": [JOB_STATE_CHANGE],
                    },
                    "Targets": [{
                        "Id": "ControlTarget",
                        "Arn": { "Fn::GetAtt": ["ControlQueue", "Arn"] },
                    }],
                },
            },
        },
        "Outputs": {
            "BucketName": { "Value": { "Ref": "Scripts" } },
            "RoleArn": { "Value": { "Fn::GetAtt": ["Role", "Arn"] } },
            "JobName": { "Value": { "Ref": "Job" } },
            "RuleArn": { "Value": { "Fn::GetAtt": ["Rule", "Arn"] } },
            "QueueUrl": { "Value": { "Ref": "Queue" } },
            "UnpermittedQueueUrl": { "Value": { "Ref": "UnpermittedQueue" } },
            "ControlQueueUrl": { "Value": { "Ref": "ControlQueue" } },
        },
    })
    .to_string()
}

/// Deploy `names.stack` and answer the outputs it published once it reports
/// [`CREATE_COMPLETE`], waiting for that STATUS rather than for a span.
async fn deployed_outputs(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
    names: &Names,
) -> BTreeMap<String, String> {
    let client = cloudformation_client(router);
    client
        .create_stack()
        .stack_name(&names.stack)
        .template_body(template(row, names))
        // "If your stack template contains IAM resources with custom names, you must specify
        // CAPABILITY_NAMED_IAM" — the stack names the job's role.
        .capabilities(aws_sdk_cloudformation::types::Capability::CapabilityNamedIam)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the stack must be accepted: {error:?}"));

    let deadline = Instant::now() + STACK_DEADLINE;
    loop {
        let described = client
            .describe_stacks()
            .stack_name(&names.stack)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the stack must be describable: {error:?}")
            });
        let stack = described
            .stacks()
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: DescribeStacks must report the stack"));
        let status = stack.stack_status().map(|status| status.as_str().to_string());
        if status.as_deref() == Some(CREATE_COMPLETE) {
            return stack
                .outputs()
                .iter()
                .filter_map(|output| {
                    Some((
                        output.output_key()?.to_string(),
                        output.output_value()?.to_string(),
                    ))
                })
                .collect();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pipeline stack to deploy, it reports {status:?} ({:?})",
            stack.stack_status_reason()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// One output of the stack, or a panic naming the case: the pipeline is unusable without it,
/// which is itself part of what the rule asks for.
fn output(case: &str, outputs: &BTreeMap<String, String>, name: &str) -> String {
    outputs.get(name).cloned().unwrap_or_else(|| {
        panic!("case {case:?}: expected the stack to publish {name}, got {outputs:?}")
    })
}

/// A string member of a response, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// Poll `GetJobRun` until the run has come to rest, exactly as a client waits for one, and
/// answer the `JobRun` it then reports.
async fn run_at_rest(router: &MultiServiceRouter, case: &str, job: &str, run_id: &str) -> Value {
    let deadline = Instant::now() + RUN_DEADLINE;
    loop {
        let reported = ok(
            router,
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
            "case {case:?}: expected the run to come to rest, it reports {run}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Every message body waiting on the queue at `url` right now, each DELETED as it is read.
///
/// Deleted, so no message can come back when its visibility timeout lapses and be counted a
/// second time: what the counts below assert is how many deliveries were really made, which a
/// message read twice — or one body collapsed with an identical one — would hide.
async fn drain(client: &aws_sdk_sqs::Client, case: &str, url: &str) -> Vec<String> {
    let received = client
        .receive_message()
        .queue_url(url)
        .max_number_of_messages(10)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the receive must answer: {error:?}"));
    let mut bodies: Vec<String> = Vec::new();
    for message in received.messages() {
        if let Some(body) = message.body() {
            bodies.push(body.to_string());
        }
        if let Some(handle) = message.receipt_handle() {
            client
                .delete_message()
                .queue_url(url)
                .receipt_handle(handle)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: the delete must answer: {error:?}")
                });
        }
    }
    bodies
}

/// Every body the queue at `url` held, waited for until one carries `needle` or
/// [`DELIVERY_DEADLINE`] passes.
async fn awaited_bodies(
    client: &aws_sdk_sqs::Client,
    case: &str,
    url: &str,
    needle: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(drain(client, case, url).await);
        if seen.iter().any(|body| body.contains(needle)) || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Every body the queue at `url` held over [`QUIET_WINDOW`], looked at repeatedly — what a
/// client polling for a message that never comes sees.
async fn quiet_bodies(client: &aws_sdk_sqs::Client, case: &str, url: &str) -> Vec<String> {
    let deadline = Instant::now() + QUIET_WINDOW;
    let mut seen: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        seen.extend(drain(client, case, url).await);
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    seen
}

/// The envelopes among `bodies` that report the run `run_id`.
fn envelopes_of_run(case: &str, bodies: &[String], run_id: &str) -> Vec<Value> {
    bodies
        .iter()
        .map(|body| {
            serde_json::from_str::<Value>(body).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: expected the delivered body to be JSON ({error}), got \
                     {body:?}"
                )
            })
        })
        .filter(|envelope| envelope.pointer("/detail/jobRunId") == Some(&json!(run_id)))
        .collect()
}

/// How many messages the queue at `url` reports holding, as that service's own attribute.
async fn approximate_messages(client: &aws_sdk_sqs::Client, case: &str, url: &str) -> String {
    let reported = client
        .get_queue_attributes()
        .queue_url(url)
        .attribute_names(aws_sdk_sqs::types::QueueAttributeName::ApproximateNumberOfMessages)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the attribute read must answer: {error:?}"));
    reported
        .attributes()
        .and_then(|attributes| {
            attributes
                .get(&aws_sdk_sqs::types::QueueAttributeName::ApproximateNumberOfMessages)
                .cloned()
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the queue to report how many messages it holds")
        })
}

/// RULE: a run of a template-provisioned job publishes the state it came to rest in on the
/// default bus, and the stack's own rule and queue policy carry that envelope to the stack's
/// queue — while a rule that cannot match, and a target whose policy does not admit the
/// delivering rule, receive nothing.
#[tokio::test]
async fn a_cfn_deployed_job_state_rule_delivers_the_run_to_its_queue() {
    // ONE emulator for the whole table: every row names its own stack and its own resources
    // inside it, so no row can see another's — and a rule an earlier row left on the bus only
    // ever delivers to that row's own queues, which have been read by the time the next row
    // starts.
    let router = emulator_with_a_stack();
    let queues = sqs_client(&router);
    let bus = events_client(&router);
    let objects = s3_client(&router);

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let names = Names::of(index);

        // ---- The stack, as a client deploys it: one template, one create, one wait.
        let outputs = deployed_outputs(&router, case, row, &names).await;
        let bucket = output(case, &outputs, "BucketName");
        let role_arn = output(case, &outputs, "RoleArn");
        let job_name = output(case, &outputs, "JobName");
        let rule_arn = output(case, &outputs, "RuleArn");
        let queue_url = output(case, &outputs, "QueueUrl");
        let unpermitted_url = output(case, &outputs, "UnpermittedQueueUrl");
        let control_url = output(case, &outputs, "ControlQueueUrl");
        for (name, actual, expected) in [
            ("BucketName", &bucket, names.bucket.clone()),
            (
                "RoleArn",
                &role_arn,
                format!("arn:aws:iam::{ACCOUNT_ID}:role/{}", names.role),
            ),
            ("JobName", &job_name, names.job.clone()),
            ("RuleArn", &rule_arn, names.rule_arn()),
        ] {
            assert_eq!(
                actual, &expected,
                "case {case:?}: expected the stack's {name} to be {expected:?}, got {actual:?}"
            );
        }

        // The job definition the template declared really is in this service, under the name
        // the stack published and with the definition the declaration carried: the run below is
        // started against this service's own resource.
        let read_back = ok(&router, "GetJob", &json!({ "JobName": &job_name })).await;
        assert_eq!(
            member(case, &read_back, "/Job/Role"),
            role_arn,
            "case {case:?}: expected GetJob to report the stack's role, got {read_back}"
        );
        assert_eq!(
            member(case, &read_back, "/Job/Command/ScriptLocation"),
            names.script_location(),
            "case {case:?}: expected GetJob to report the declared script location, got \
             {read_back}"
        );

        // The rule's inline `Targets` really reached EventBridge: a rule with no target
        // delivers nowhere however well the envelope matches.
        let installed = bus
            .list_targets_by_rule()
            .rule(&names.rule)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the rule's targets must be readable: {error:?}")
            });
        let targets: Vec<(String, String)> = installed
            .targets()
            .iter()
            .map(|target| (target.id().to_string(), target.arn().to_string()))
            .collect();
        assert_eq!(
            targets,
            vec![
                (PERMITTED_TARGET_ID.to_string(), arn("sqs", &names.queue)),
                (
                    UNPERMITTED_TARGET_ID.to_string(),
                    arn("sqs", &names.unpermitted_queue)
                ),
            ],
            "case {case:?}: expected both declared targets on rule {:?}, got {targets:?}",
            names.rule
        );

        // ---- The run, started as the issue's client starts it: the script is staged in the
        // stack's bucket, then one `StartJobRun` against the stack's job.
        objects
            .put_object()
            .bucket(&bucket)
            .key(SCRIPT_KEY)
            .body(aws_sdk_s3::primitives::ByteStream::from_static(
                row.script.as_bytes(),
            ))
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the script must be staged: {error:?}"));
        let started = ok(&router, "StartJobRun", &json!({ "JobName": &job_name })).await;
        let run_id = member(case, &started, "/JobRunId");

        // The run reaches the terminal state its own script leads it to, which is the state the
        // envelope must carry.
        let run = run_at_rest(&router, case, &job_name, &run_id).await;
        assert_eq!(
            member(case, &run, "/JobRunState"),
            row.run_state,
            "case {case:?}: expected the run to come to rest in {:?}, got {run}",
            row.run_state
        );

        // ---- The control: one rule of the SAME stack, matching the source and detail type
        // alone, whose target is the control queue. Its message is what says the envelope was
        // published and dispatched at all.
        let control = awaited_bodies(&queues, case, &control_url, &run_id).await;
        let control_envelopes = envelopes_of_run(case, &control, &run_id);
        assert_eq!(
            control_envelopes.len(),
            1,
            "case {case:?}: expected the control queue to receive exactly one envelope of run \
             {run_id:?}, got {control:?}"
        );

        // The envelope is the whole one a target is handed, and its detail is this run's own
        // stored state — the values the control plane reports for the same run.
        let envelope = control_envelopes[0].clone();
        let expected_severity = if row.run_state == FAILED {
            SEVERITY_ERROR
        } else {
            SEVERITY_INFO
        };
        for (pointer, expect) in [
            ("/source", json!(EVENT_SOURCE)),
            ("/detail-type", json!(JOB_STATE_CHANGE)),
            ("/account", json!(ACCOUNT_ID)),
            ("/region", json!(REGION)),
            ("/detail/jobName", json!(&job_name)),
            ("/detail/jobRunId", json!(&run_id)),
            ("/detail/state", json!(row.run_state)),
            ("/detail/severity", json!(expected_severity)),
        ] {
            let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expect,
                "case {case:?}: expected {pointer} to be {expect}, got {actual} in {envelope}"
            );
        }
        assert!(
            envelope["id"].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the envelope to carry an event id, got {envelope}"
        );
        assert!(
            envelope["time"].as_str().is_some_and(|at| at.ends_with('Z')),
            "case {case:?}: expected the envelope to state when the run came to rest, got \
             {envelope}"
        );
        // The reason the envelope reports is the run's OWN reason, the one `GetJobRun` answers
        // with, and a run that failed at nothing reports none.
        assert_eq!(
            envelope.pointer("/detail/message").and_then(Value::as_str),
            run.get("ErrorMessage").and_then(Value::as_str),
            "case {case:?}: expected the envelope's message to be the run's own, got {envelope} \
             for {run}"
        );
        assert_eq!(
            envelope.pointer("/detail/message").is_some(),
            row.run_state == FAILED,
            "case {case:?}: expected a message={}, got {envelope}",
            row.run_state == FAILED
        );

        // ---- What a client reads off the stack's own queue.
        if row.delivered {
            let bodies = awaited_bodies(&queues, case, &queue_url, &run_id).await;
            let delivered = envelopes_of_run(case, &bodies, &run_id);
            assert_eq!(
                delivered.len(),
                1,
                "case {case:?}: expected exactly one envelope of run {run_id:?} on the stack's \
                 queue, got {bodies:?}"
            );
            assert_eq!(
                delivered[0], envelope,
                "case {case:?}: expected the target to be handed the envelope UNCHANGED, got {}",
                delivered[0]
            );
        } else {
            // The control proves the publication and the dispatch happened; the row's own queue
            // is then looked at repeatedly, and must hold nothing of this run.
            let quiet = quiet_bodies(&queues, case, &queue_url).await;
            assert!(
                envelopes_of_run(case, &quiet, &run_id).is_empty(),
                "case {case:?}: expected the stack's queue to receive nothing of run \
                 {run_id:?}, got {quiet:?}"
            );
        }

        // The sibling target that granted the service principal nothing receives nothing at
        // all, and reports holding nothing — the permission boundary, read as a client reads it.
        let withheld = quiet_bodies(&queues, case, &unpermitted_url).await;
        assert!(
            withheld.is_empty(),
            "case {case:?}: expected the queue with no resource policy to receive nothing, got \
             {withheld:?}"
        );
        let held = approximate_messages(&queues, case, &unpermitted_url).await;
        assert_eq!(
            held, "0",
            "case {case:?}: expected the queue with no resource policy to report 0 messages, \
             got {held:?}"
        );
    }
}
