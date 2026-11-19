// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the CloudWatch Logs GROUP and STREAM a training job's
//! algorithm container writes to.
//!
//! THE RULE UNDER TEST: a SageMaker job whose container SageMaker runs on the account's
//! behalf has its container output published to CloudWatch Logs, so the group and the stream
//! that output goes to are CloudWatch Logs' own resources from the moment the container
//! starts. For a training job that is the group `/aws/sagemaker/TrainingJobs` and the stream
//! `[training-job-name]/algo-[container]-[epoch timestamp]` — the layout AWS documents for a
//! training job's logs. A client therefore lists the streams of a job that has trained
//! (`DescribeLogStreams` answering 200), reads one (`GetLogEvents`), and reads an empty page
//! for a prefix no stream matches — while `ResourceNotFoundException` is left for a group
//! nobody published.
//!
//! What the stream HOLDS is whatever the container wrote: this emulator starts no algorithm
//! image, so a job that declares no container program of its own has an empty stream and no
//! log event is invented for it.
//!
//! SageMaker, Amazon S3 and CloudWatch Logs are mounted on ONE shared engine, the way
//! `core/router` mounts every service, and every call goes over the real wire through
//! `MultiServiceRouter::dispatch` (`awsJson1_1` with `X-Amz-Target` for SageMaker and
//! CloudWatch Logs, path-style requests for Amazon S3), so what is asserted is what an SDK
//! client sees. IAM is deliberately NOT mounted: what the execution role may read and write
//! is `training_job_execution.rs`'s subject, and nothing is withheld from a role this
//! emulator was never told about.
//!
//! The table spans what the rule generalises over: the FORM of the training job's name (a
//! single character, the timestamped name an SDK mints, the punctuation a name may carry),
//! how many input channels the job declares (none, one, two) and how many records they hold.

use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::training_job_execution::{channel, training_body};

/// The region every call is signed for.
const REGION: &str = "us-east-1";

/// The bucket every case stages its channel objects in and writes its artifact to.
const BUCKET: &str = "sagemaker-training-job-logs";

/// The log group SageMaker publishes a training job's container logs in, read from the
/// state's own constant so the suite and the service cannot disagree about it.
const LOG_GROUP: &str = service_com_amazonaws_sagemaker_sagemaker::state::TRAINING_JOB_LOG_GROUP;

/// The execution role every case's job runs as. No IAM is mounted, so nothing is withheld
/// from it (see this module's header).
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sagemaker-training-job-logs";

/// A SigV4 credential scope naming `service`: it is what routes a request to that service.
/// The key id and signature are not credentials — this harness turns request authentication
/// off, so nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// SageMaker, Amazon S3 and CloudWatch Logs on ONE shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One `awsJson1_1` call to the service `target_prefix` names.
    async fn call(
        &self,
        target_prefix: &str,
        service: &str,
        target: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{target_prefix}.{target}"))
            .header("authorization", authorization(service))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One SageMaker call, as a client makes it.
    async fn sagemaker(&self, target: &str, body: &Value) -> (u16, Value) {
        self.call("SageMaker", "sagemaker", target, body).await
    }

    /// One SageMaker call that must be served.
    async fn sagemaker_ok(&self, case: &str, target: &str, body: &Value) -> Value {
        let (status, answered) = self.sagemaker(target, body).await;
        assert_eq!(
            status, 200,
            "case {case}: expected {target} to answer HTTP 200, got {status} with {answered}"
        );
        answered
    }

    /// One CloudWatch Logs call, as a client makes it.
    async fn logs(&self, target: &str, body: &Value) -> (u16, Value) {
        self.call("Logs_20140328", "logs", target, body).await
    }

    /// One raw path-style request to the emulator's own Amazon S3 front door.
    async fn s3(&self, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", authorization("s3"))
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.body().to_vec())
    }

    /// `PUT /<bucket>` — `CreateBucket`.
    async fn given_bucket(&self, bucket: &str) {
        let (status, body) = self.s3("PUT", &format!("/{bucket}"), &[]).await;
        assert_eq!(
            status,
            200,
            "the bucket {bucket} must be created, got {status} {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// `PUT /<bucket>/<key>` — `PutObject`, storing `body` as the object's own bytes.
    async fn given_object(&self, bucket: &str, key: &str, body: &str) {
        let (status, answered) = self
            .s3("PUT", &format!("/{bucket}/{key}"), body.as_bytes())
            .await;
        assert_eq!(
            status,
            200,
            "the object {key} must be stored, got {status} {}",
            String::from_utf8_lossy(&answered)
        );
    }

    /// `DescribeLogStreams` over [`LOG_GROUP`], optionally narrowed by `prefix`.
    async fn describe_log_streams(&self, prefix: Option<&str>) -> (u16, Value) {
        let mut body = json!({ "logGroupName": LOG_GROUP });
        if let Some(prefix) = prefix {
            body["logStreamNamePrefix"] = json!(prefix);
        }
        self.logs("DescribeLogStreams", &body).await
    }

    /// The names of the streams `DescribeLogStreams` reports for `prefix`, asserting the
    /// read itself is served.
    async fn stream_names(&self, case: &str, prefix: Option<&str>) -> Vec<String> {
        let (status, answered) = self.describe_log_streams(prefix).await;
        assert_eq!(
            status, 200,
            "case {case}: expected DescribeLogStreams for prefix {prefix:?} to answer HTTP \
             200, got {status} with {answered}"
        );
        answered
            .get("logStreams")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!("case {case}: DescribeLogStreams reports logStreams, got {answered}")
            })
            .iter()
            .map(|stream| {
                stream
                    .get("logStreamName")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case}: a log stream carries logStreamName, got {stream}")
                    })
                    .to_string()
            })
            .collect()
    }
}

/// The wire error code of an awsJson error body (`__type`, which may carry a namespace).
fn error_code(body: &Value) -> String {
    let raw = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("an error body must carry __type, got {body}"));
    raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string()
}

/// One row of the table: a job name's FORM, the channels the job declares and the records
/// each of them holds.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    job_name: &'static str,
    /// One entry per input channel, each the records that channel's object holds.
    channels: &'static [&'static str],
}

/// The rows: the forms a `TrainingJobName` takes and the channel sets a job declares, from
/// none at all through two.
const ROWS: &[Row] = &[
    Row {
        case: "a single-character job name, one channel",
        job_name: "t",
        channels: &["1,2\n3,4\n5,6\n"],
    },
    Row {
        case: "the timestamped name an SDK mints",
        job_name: "sagemaker-xgboost-2026-11-09-12-37-45-123",
        channels: &["0.5,1\n1.5,2\n"],
    },
    Row {
        // 63 characters, which is the ceiling `TrainingJobName`'s own pattern states
        // (`[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}`), in both letter cases and with the one
        // punctuation character that pattern admits.
        case: "a mixed-case name at the 63-character ceiling",
        job_name: "Aa0123456789-0123456789-0123456789-0123456789-0123456789-012345",
        channels: &["7,8\n"],
    },
    Row {
        case: "two channels",
        job_name: "two-channel-job",
        channels: &["1,1\n2,2\n3,3\n", "4,4\n5,5\n"],
    },
    Row {
        case: "no input channels at all",
        job_name: "no-channel-job",
        channels: &[],
    },
];

/// Stage `row`'s channel objects and create its training job, answering the body the create
/// was given.
async fn given_trained_job(emulator: &Emulator, row: Row) -> Value {
    let mut channels: Vec<Value> = Vec::new();
    for (index, records) in row.channels.iter().enumerate() {
        let key = format!("{}/channel-{index}.csv", row.job_name);
        emulator.given_object(BUCKET, &key, records).await;
        channels.push(channel(
            if index == 0 { "train" } else { "validation" },
            &format!("s3://{BUCKET}/{key}"),
            "S3Prefix",
        ));
    }
    let mut body = training_body(
        row.job_name,
        ROLE_ARN,
        json!(channels),
        &format!("s3://{BUCKET}/output"),
    );
    if row.channels.is_empty() {
        // `InputDataConfig` is modelled `len 1..20`, so a job that declares no channel at
        // all leaves the member out rather than sending an empty list.
        body.as_object_mut()
            .expect("a CreateTrainingJob body is an object")
            .remove("InputDataConfig");
    }
    emulator
        .sagemaker_ok(row.case, "CreateTrainingJob", &body)
        .await;
    body
}

/// The stream `names` holds for the container `container` of `job`, asserting its shape:
/// the job's own name, the container, and the epoch second the container started at.
fn container_stream(case: &str, names: &[String], job: &str, container: &str) -> String {
    let prefix = format!("{job}/{container}-");
    let found: Vec<&String> = names
        .iter()
        .filter(|name| name.starts_with(&prefix))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case}: expected exactly one stream named {prefix:?}<epoch second>, got {names:?}"
    );
    let name = found[0].clone();
    let stamp = name
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("case {case}: {name:?} starts with {prefix:?}"));
    assert!(
        !stamp.is_empty() && stamp.chars().all(|digit| digit.is_ascii_digit()),
        "case {case}: expected {name:?} to end in the epoch second the container started \
         at, got {stamp:?}"
    );
    name
}

/// Every trained job publishes the stream its algorithm container writes to, in the ONE
/// group SageMaker publishes training-job logs in — whatever the job's name and channels.
#[tokio::test]
async fn a_trained_job_publishes_its_container_stream() {
    for row in ROWS {
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        given_trained_job(&emulator, *row).await;

        // The group itself is CloudWatch Logs', so that service reports it.
        let (status, groups) = emulator
            .logs(
                "DescribeLogGroups",
                &json!({ "logGroupNamePrefix": LOG_GROUP }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {}: DescribeLogGroups, got {status} with {groups}",
            row.case
        );
        let held: Vec<String> = groups
            .get("logGroups")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|group| group.get("logGroupName").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect();
        assert!(
            held.iter().any(|name| name == LOG_GROUP),
            "case {}: expected CloudWatch Logs to hold {LOG_GROUP}, got {held:?}",
            row.case
        );

        let names = emulator
            .stream_names(row.case, Some(&format!("{}/", row.job_name)))
            .await;
        let stream = container_stream(row.case, &names, row.job_name, "algo-1");

        // The stream is the container's, and this emulator started no image inside it, so it
        // holds no log event: an empty read, never an invented line.
        let (status, events) = emulator
            .logs(
                "GetLogEvents",
                &json!({ "logGroupName": LOG_GROUP, "logStreamName": stream }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {}: expected GetLogEvents on {stream} to answer HTTP 200, got {status} \
             with {events}",
            row.case
        );
        let read: &Vec<Value> = events
            .get("events")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {}: GetLogEvents reports events, got {events}", row.case));
        assert!(
            read.is_empty(),
            "case {}: expected the stream of a container that wrote nothing to hold no \
             event, got {read:?}",
            row.case
        );
    }
}

/// ONE group holds every job's streams, and a prefix no stream matches is an empty page
/// rather than a refusal — the empty-list answer a client reads for a group it holds.
#[tokio::test]
async fn one_group_holds_every_job_and_an_unmatched_prefix_is_an_empty_page() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let mut expected: Vec<String> = Vec::new();
    for row in ROWS {
        given_trained_job(&emulator, *row).await;
        let names = emulator
            .stream_names(row.case, Some(&format!("{}/", row.job_name)))
            .await;
        expected.push(container_stream(row.case, &names, row.job_name, "algo-1"));
    }

    let every = emulator.stream_names("every job", None).await;
    for stream in &expected {
        assert!(
            every.contains(stream),
            "every job: expected {LOG_GROUP} to hold {stream:?}, got {every:?}"
        );
    }

    for prefix in [
        "no-such-job/".to_string(),
        // The containers of a cluster this emulator ran none of, and a stamp no stream
        // carries: each is a prefix inside a group that EXISTS, so each is an empty page.
        format!("{}/algo-2-", ROWS[0].job_name),
        format!("{}/algo-1-00", ROWS[0].job_name),
    ] {
        let names = emulator.stream_names(&prefix, Some(&prefix)).await;
        assert!(
            names.is_empty(),
            "case {prefix:?}: expected an empty page for a prefix no stream matches, got \
             {names:?}"
        );
    }
}

/// A job that never trained publishes no stream: a create refused for an input location
/// holding no object leaves the group without one, and a group nobody published is the only
/// `ResourceNotFoundException`.
#[tokio::test]
async fn a_job_that_never_trained_publishes_no_stream() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;

    // Nothing has trained yet, so CloudWatch Logs holds no such group at all — which is the
    // one case the refusal is for.
    let (status, refused) = emulator.describe_log_streams(None).await;
    assert_eq!(
        status, 400,
        "a group nobody published: expected HTTP 400, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ResourceNotFoundException",
        "a group nobody published: expected ResourceNotFoundException, got {refused}"
    );

    // An input channel whose location holds no object backs no run, so the create is refused
    // and no container of this job ever starts.
    let refused_name = "refused-job";
    let (status, answered) = emulator
        .sagemaker(
            "CreateTrainingJob",
            &training_body(
                refused_name,
                ROLE_ARN,
                json!([channel(
                    "train",
                    &format!("s3://{BUCKET}/nothing-is-here/"),
                    "S3Prefix",
                )]),
                &format!("s3://{BUCKET}/output"),
            ),
        )
        .await;
    assert_eq!(
        status, 400,
        "a channel holding no object: expected HTTP 400, got {status} with {answered}"
    );

    // A job that DID train publishes its own stream, and the refused job still has none.
    given_trained_job(&emulator, ROWS[0]).await;
    let names = emulator.stream_names("after one trained job", None).await;
    container_stream("after one trained job", &names, ROWS[0].job_name, "algo-1");
    assert!(
        !names.iter().any(|name| name.starts_with(refused_name)),
        "expected no stream for a job that never trained, got {names:?}"
    );
}

/// The stream name is derived from the job and the instant its container started, not from
/// anything a request carries: two jobs of the same emulator get two streams, each named for
/// its own job.
#[tokio::test]
async fn each_job_gets_its_own_stream_name() {
    let before = SystemTime::now();
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    given_trained_job(&emulator, ROWS[0]).await;
    given_trained_job(&emulator, ROWS[1]).await;

    let names = emulator.stream_names("two jobs", None).await;
    let first = container_stream("two jobs", &names, ROWS[0].job_name, "algo-1");
    let second = container_stream("two jobs", &names, ROWS[1].job_name, "algo-1");
    assert_ne!(
        first, second,
        "two jobs: expected each job's own stream, got {names:?}"
    );

    // The stamp each stream carries is the second its own container started at, so it is no
    // earlier than the instant this test began.
    let started = before
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("the test runs after the epoch")
        .as_secs();
    for (name, job) in [(&first, ROWS[0].job_name), (&second, ROWS[1].job_name)] {
        let stamp: u64 = name
            .rsplit('-')
            .next()
            .and_then(|digits| digits.parse().ok())
            .unwrap_or_else(|| panic!("{job}: {name:?} ends in an epoch second"));
        assert!(
            stamp >= started,
            "{job}: expected a stamp no earlier than {started}, got {stamp} in {name:?}"
        );
    }
}
