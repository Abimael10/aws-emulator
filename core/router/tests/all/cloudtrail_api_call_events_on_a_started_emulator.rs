// SPDX-License-Identifier: CC0-1.0

//! The CloudTrail management-event fan-out, driven against the emulator a customer STARTS:
//! the built binary, on its own port, with request authentication on, reached over TLS with
//! SigV4-signed calls.
//!
//! RULE: while a trail of the Region is logging management events of a call's read/write
//! class, every control-plane call the account serves — whichever service serves it — is
//! ALSO published onto that Region's DEFAULT event bus, with `source` `aws.<endpoint-prefix>`
//! of the serving service, `detail-type` `AWS API Call via CloudTrail` and the CloudTrail
//! record of the call as `detail`; a rule whose pattern names `source`, `detail-type`,
//! `detail.eventSource` or `detail.eventName` is handed exactly the events that match it, and
//! that holds on the emulator as it is STARTED, not only on a router a test assembles in its
//! own process.
//!
//! The sibling suite `all/cloudtrail_dynamodb_api_call_events` spans what the rule
//! generalises over on a router built inside the test process, with authentication off. The
//! emulator a client runs is a different configuration of the same code: the credentials are
//! verified, each call arrives over its own TLS connection, every nested delivery the fan-out
//! makes runs on the binary's own multi-thread runtime, and one process serves every call.
//! This suite is the one that pins the fan-out THERE, so a channel that delivers in-process
//! and not on the started emulator is a failure a test reports rather than one only a client
//! sees. ONE wiring is driven here, since starting the binary costs what starting the
//! emulator costs.
//!
//! Every assertion quotes the emulator's own log, which is where a delivery that could not be
//! made says so.
//!
//! The start-and-sign harness below is this module's own, as it is in each of the sibling
//! suites that start the same binary (`all/ddb_stream_lambda_esm_filter_criteria_on_a_started_emulator`,
//! `all/cfn_deployed_pipeline_on_a_started_emulator`): each of those holds its copy private to
//! its own module, so there is nothing a third suite can reach for.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The Region every call below is signed for, and so the Region the trail, the queues, the
/// rules and the table belong to.
const REGION: &str = "us-east-1";

/// The account every emulated resource belongs to (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The `detail-type` CloudTrail's management-event fan-out carries
/// (`service_common::api_call_events::API_CALL_DETAIL_TYPE`).
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";

/// The `source` and the record `eventSource` of each service whose calls are published here.
const DYNAMODB_EVENT_SOURCE: &str = "aws.dynamodb";
const DYNAMODB_RECORD_SOURCE: &str = "dynamodb.amazonaws.com";
const SQS_EVENT_SOURCE: &str = "aws.sqs";
const SQS_RECORD_SOURCE: &str = "sqs.amazonaws.com";

/// The table the control-plane writes below create and delete.
const TABLE: &str = "started-emulator-fan-out";

/// The trail and the bucket it delivers into.
const TRAIL: &str = "started-emulator-fan-out";
const BUCKET: &str = "started-emulator-fan-out-trail";

/// How long a start may take to publish its files, and how long a delivery or a table's own
/// transition is waited for. Every wait is on a CONDITION; the spans are generous, so a
/// loaded host spends none of them.
const STARTUP_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// Ends an emulator a crashed test left behind, as `tests/emulator_core` does.
const ORPHAN_IDLE_TIMEOUT_SECS: &str = "120";

/// How long the client waits on one `ReceiveMessage`, in seconds — "The duration (in
/// seconds) for which the call waits for a message to arrive in the queue before returning"
/// (`ReceiveMessageRequest$WaitTimeSeconds`). A long poll is how a client waits for a
/// delivery it cannot otherwise be told about, which is the wait a consumer of this channel
/// makes.
const WAIT_TIME_SECONDS: u64 = 2;

/// A running emulator, killed and its home folder removed once dropped, however a test ends.
struct Started {
    child: Child,
    home: PathBuf,
    folder: PathBuf,
    port: u16,
    client: reqwest::blocking::Client,
    key: String,
    secret: String,
}

impl Drop for Started {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Started {
    /// Start the built binary on a free port in a home folder of its own, and wait until it
    /// has published the files a client reads its port, its certificate and its credentials
    /// from.
    fn start() -> Self {
        let home = new_home();
        let log = std::fs::File::create(home.join("emulator.log")).expect("create the log file");
        let mut command = Command::new(emulator_binary());
        command.env_clear().env("HOME", &home);
        command.envs(std::env::var_os("PATH").map(|path| ("PATH", path)));
        command.env("EMULATOR_TELEMETRY", "off");
        command.args(["-p", "0", "--idle-timeout", ORPHAN_IDLE_TIMEOUT_SECS]);
        command.stdin(Stdio::null());
        command.stderr(log.try_clone().expect("duplicate the log file"));
        let child = command.stdout(log).spawn().expect("start the emulator binary");
        let mut started = Self {
            child,
            home,
            folder: PathBuf::new(),
            port: 0,
            client: reqwest::blocking::Client::new(),
            key: String::new(),
            secret: String::new(),
        };
        let giving_up_at = Instant::now() + STARTUP_DEADLINE;
        let folder = loop {
            if let Some(status) = started.child.try_wait().expect("read the emulator's state") {
                panic!(
                    "the emulator exited with {status}; its log:\n{}",
                    started.log()
                );
            }
            if let Some(folder) = folder_publishing_config(&started.home, 4) {
                break folder;
            }
            assert!(
                Instant::now() < giving_up_at,
                "the emulator published no files inside {STARTUP_DEADLINE:?}; its log:\n{}",
                started.log()
            );
            std::thread::sleep(LOOK_INTERVAL);
        };
        started.port = folder
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.parse().ok())
            .unwrap_or_else(|| panic!("{folder:?} names no port"));
        started.folder = folder;
        let credentials = std::fs::read_to_string(started.file("emulator_credentials"))
            .expect("read the published credentials");
        started.key = profile_value(&credentials, "aws_access_key_id")
            .expect("the published access key id");
        started.secret = profile_value(&credentials, "aws_secret_access_key")
            .expect("the published secret");
        started.client = trusting_client(&started.file("ca.pem"));
        started
    }

    fn file(&self, name: &str) -> PathBuf {
        self.folder.join(name)
    }

    /// Everything the emulator has written to its own log, which is where a delivery that
    /// could not be made, or a call it refused, says so.
    fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("emulator.log")).unwrap_or_default()
    }

    /// One signed call, answered with its status and body text.
    fn call(
        &self,
        signing_name: &str,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> (u16, String) {
        let uri = format!("https://127.0.0.1:{}{path}", self.port);
        let mut request = http::Request::builder().method(method).uri(uri);
        request = request.header("host", format!("{signing_name}.{REGION}.amazonaws.com"));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let digest = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&body));
        request = request.header("x-amz-content-sha256", digest);
        let request = request.body(body).expect("a well-formed request");
        let signed = self.signed(request, signing_name);
        let response = self
            .client
            .execute(signed.try_into().expect("a request reqwest can send"))
            .expect("the emulator answers");
        let status = response.status().as_u16();
        let body = response.text().unwrap_or_default();
        (status, body)
    }

    /// `request` signed with the published key pair for `signing_name`, as an SDK signs it.
    fn signed(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
        use aws_sigv4::sign::v4::SigningParams;

        let identity = aws_credential_types::Credentials::new(
            &self.key,
            &self.secret,
            None,
            None,
            "the published key pair",
        )
        .into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(std::time::SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("signing parameters")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let declared = headers
            .iter()
            .find(|(name, _)| name == "x-amz-content-sha256")
            .map(|(_, value)| value.clone());
        let borrowed = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()));
        let bytes = SignableBody::Bytes(request.body());
        let body = declared.map_or(bytes, SignableBody::Precomputed);
        let signable =
            SignableRequest::new(&method, &uri, borrowed, body).expect("a signable request");
        let (instructions, _) = sign(signable, &params).expect("a signature").into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }

    /// One `X-Amz-Target` RPC call, parsed as JSON.
    fn rpc(
        &self,
        signing_name: &str,
        target: &str,
        json_version: &str,
        body: &Value,
    ) -> (u16, Value) {
        let (status, text) = self.call(
            signing_name,
            "POST",
            "/",
            &[
                (
                    "content-type",
                    &format!("application/x-amz-json-{json_version}"),
                ),
                ("x-amz-target", target),
            ],
            body.to_string().into_bytes(),
        );
        (status, parsed(&text))
    }

    /// One Amazon DynamoDB call, which must answer 200.
    fn dynamodb(&self, operation: &str, body: &Value) -> Value {
        let (status, answered) =
            self.rpc("dynamodb", &format!("DynamoDB_20120810.{operation}"), "1.0", body);
        assert_eq!(
            status, 200,
            "DynamoDB {operation} must answer 200, got {status} with {answered}; the log:\n{}",
            self.log()
        );
        answered
    }

    /// One Amazon SQS call, which must answer 200.
    fn sqs(&self, operation: &str, body: &Value) -> Value {
        let (status, answered) =
            self.rpc("sqs", &format!("AmazonSQS.{operation}"), "1.0", body);
        assert_eq!(
            status, 200,
            "Amazon SQS {operation} must answer 200, got {status} with {answered}; the log:\n{}",
            self.log()
        );
        answered
    }

    /// One EventBridge call, which must answer 200.
    fn events(&self, operation: &str, body: &Value) -> Value {
        let (status, answered) =
            self.rpc("events", &format!("AWSEvents.{operation}"), "1.1", body);
        assert_eq!(
            status, 200,
            "EventBridge {operation} must answer 200, got {status} with {answered}; the log:\n{}",
            self.log()
        );
        answered
    }

    /// One CloudTrail call, which must answer 200.
    fn cloudtrail(&self, operation: &str, body: &Value) -> Value {
        let (status, answered) = self.rpc(
            "cloudtrail",
            &format!("CloudTrail_20131101.{operation}"),
            "1.1",
            body,
        );
        assert_eq!(
            status, 200,
            "CloudTrail {operation} must answer 200, got {status} with {answered}; the log:\n{}",
            self.log()
        );
        answered
    }

    /// The trail's destination bucket, with the policy that lets CloudTrail write into it.
    fn create_bucket(&self) {
        let (status, body) = self.call(
            "s3",
            "PUT",
            &format!("/{BUCKET}"),
            &[("content-type", "application/xml")],
            Vec::new(),
        );
        assert_eq!(
            status, 200,
            "CreateBucket({BUCKET:?}) must succeed, got {status} with {body}"
        );
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "AWSCloudTrailWrite",
                "Effect": "Allow",
                "Principal": { "Service": "cloudtrail.amazonaws.com" },
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
                "Condition": {
                    "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" },
                },
            }],
        })
        .to_string();
        let (status, body) = self.call(
            "s3",
            "PUT",
            &format!("/{BUCKET}?policy"),
            &[("content-type", "application/json")],
            policy.into_bytes(),
        );
        assert!(
            (200..300).contains(&status),
            "PutBucketPolicy({BUCKET:?}) must succeed, got {status} with {body}"
        );
    }

    /// Create a queue and hand back its url and ARN.
    fn create_queue(&self, name: &str) -> (String, String) {
        let created = self.sqs("CreateQueue", &json!({ "QueueName": name }));
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("expected a QueueUrl for {name:?}, got {created}"))
            .to_string();
        (url, format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}"))
    }

    /// Let `rule_arn` — and only it — send to the queue at `url`.
    fn admitting_rule(&self, url: &str, queue_arn: &str, rule_arn: &str) {
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": "events.amazonaws.com" },
                "Action": "sqs:SendMessage",
                "Resource": queue_arn,
                "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
            }],
        });
        self.sqs(
            "SetQueueAttributes",
            &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
        );
    }

    /// Long-poll `url` until it has handed back at least `wanted` envelopes, or
    /// [`SETTLE_DEADLINE`] has passed — whichever comes first, and then ONCE more. The
    /// CONDITION is what is waited for; the deadline only bounds a wait that will never be
    /// satisfied.
    ///
    /// The extra look is what lets a row read "exactly one envelope": a SECOND envelope for
    /// the same rule arrives a moment after the first, so a drain that stopped at the one it
    /// was waiting for could not tell one delivery from two.
    fn delivered(&self, url: &str, wanted: usize) -> Vec<Value> {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        let mut seen: Vec<Value> = Vec::new();
        loop {
            let answer = self.sqs(
                "ReceiveMessage",
                &json!({
                    "QueueUrl": url,
                    "MaxNumberOfMessages": 10,
                    "WaitTimeSeconds": WAIT_TIME_SECONDS,
                }),
            );
            let enough = match answer["Messages"].as_array() {
                None => seen.len() >= wanted,
                Some(messages) => {
                    for message in messages {
                        let body = message["Body"].as_str().unwrap_or_default();
                        seen.push(serde_json::from_str(body).unwrap_or_else(|error| {
                            panic!("a delivered envelope is JSON ({error}): {body}")
                        }));
                    }
                    // A look that handed something back is never the last one: the next one
                    // is what says no further envelope followed it.
                    false
                }
            };
            if enough || Instant::now() >= deadline {
                return seen;
            }
        }
    }

    /// Look at the table's status until it reports `wanted`, or [`SETTLE_DEADLINE`] has
    /// passed. A create reaches `ACTIVE` on its own clock, so the CONDITION is waited for.
    fn table_status(&self, wanted: &str) -> String {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let described = self.dynamodb("DescribeTable", &json!({ "TableName": TABLE }));
            let status = described["Table"]["TableStatus"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if status == wanted || Instant::now() >= deadline {
                return status;
            }
            std::thread::sleep(LOOK_INTERVAL);
        }
    }
}

/// `text` as JSON, or null for a body that is not JSON at all.
fn parsed(text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// The binary Cargo built for this package, named at COMPILE time rather than looked up
/// among the variables of the running process, as the sibling suites that start the same
/// binary name it.
fn emulator_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aws-emulator"))
}

/// A new folder to serve as an emulator's home: never one another user created first.
fn new_home() -> PathBuf {
    static STARTS: AtomicU32 = AtomicU32::new(0);
    let name = format!("emulator-api-call-events-{}", std::process::id());
    let home = std::env::temp_dir().join(format!(
        "{name}-{}",
        STARTS.fetch_add(1, Ordering::Relaxed)
    ));
    let mut owner_only = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut owner_only, 0o700);
    owner_only
        .create(&home)
        .map(|()| home)
        .expect("create the emulator's home folder")
}

/// The value of `name` in an AWS shared credentials file.
fn profile_value(text: &str, name: &str) -> Option<String> {
    let mut pairs = text.lines().filter_map(|line| line.split_once('='));
    let (_, value) = pairs.find(|(key, _)| key.trim() == name)?;
    Some(value.trim().to_string())
}

/// A blocking HTTPS client trusting only the certificate this start published.
fn trusting_client(ca_pem: &Path) -> reqwest::blocking::Client {
    use rustls::pki_types::pem::PemObject;

    let mut roots = rustls::RootCertStore::empty();
    let certificate =
        rustls::pki_types::CertificateDer::from_pem_file(ca_pem).expect("read the certificate");
    roots.add(certificate).expect("trust the certificate");
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(rustls::ALL_VERSIONS)
        .expect("a TLS configuration");
    let tls = builder.with_root_certificates(roots).with_no_client_auth();
    reqwest::blocking::Client::builder()
        .use_preconfigured_tls(tls)
        .timeout(SETTLE_DEADLINE)
        .build()
        .expect("build the client")
}

/// The folder under `folder` holding a fully written `config`, published last, and not a
/// link.
fn folder_publishing_config(folder: &Path, depth_left: usize) -> Option<PathBuf> {
    let config = folder.join("config");
    let is_file = std::fs::symlink_metadata(&config).is_ok_and(|file| file.is_file());
    if is_file && std::fs::read_to_string(config).is_ok_and(|text| text.ends_with('\n')) {
        return Some(folder.to_path_buf());
    }
    let entries = std::fs::read_dir(folder).ok().filter(|_| depth_left > 0)?;
    let is_folder = |entry: &std::fs::DirEntry| entry.file_type().is_ok_and(|kind| kind.is_dir());
    let mut folders = entries.flatten().filter(is_folder);
    folders.find_map(|entry| folder_publishing_config(&entry.path(), depth_left - 1))
}

/// One rule this suite installs: the pattern it matches with, and which call's record must
/// reach it.
struct Row {
    case: &'static str,
    pattern: Value,
    expects: (&'static str, &'static str),
}

/// The rules, each on its own queue so what arrived at one is never read as another's: the
/// control write of a service that has bus events of its own, the subject create of a control
/// plane that has none, and the near-miss rule of that same service pinned to its delete.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the control write",
            pattern: json!({
                "source": [SQS_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [SQS_RECORD_SOURCE],
                    "eventName": ["TagQueue"],
                },
            }),
            expects: (SQS_EVENT_SOURCE, "TagQueue"),
        },
        Row {
            case: "the subject create",
            pattern: json!({
                "source": [DYNAMODB_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [DYNAMODB_RECORD_SOURCE],
                    "eventName": ["CreateTable"],
                },
            }),
            expects: (DYNAMODB_EVENT_SOURCE, "CreateTable"),
        },
        Row {
            case: "the near-miss delete",
            pattern: json!({
                "source": [DYNAMODB_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [DYNAMODB_RECORD_SOURCE],
                    "eventName": ["DeleteTable"],
                },
            }),
            expects: (DYNAMODB_EVENT_SOURCE, "DeleteTable"),
        },
    ]
}

#[test]
fn cloudtrail_sourced_api_call_events_reach_their_rules_on_a_started_emulator() {
    let started = Started::start();

    // A trail of the account, logging the WRITE management events of its own Region.
    started.create_bucket();
    started.cloudtrail(
        "CreateTrail",
        &json!({ "Name": TRAIL, "S3BucketName": BUCKET }),
    );
    started.cloudtrail(
        "PutEventSelectors",
        &json!({
            "TrailName": TRAIL,
            "EventSelectors": [{ "ReadWriteType": "WriteOnly", "IncludeManagementEvents": true }],
        }),
    );
    started.cloudtrail("StartLogging", &json!({ "Name": TRAIL }));
    let status = started.cloudtrail("GetTrailStatus", &json!({ "Name": TRAIL }));
    assert_eq!(
        status["IsLogging"],
        json!(true),
        "the trail must be logging, got {status}; the log:\n{}",
        started.log()
    );

    // One rule per row, each on its own queue, all on the DEFAULT bus — the only bus an
    // AWS-published event reaches. The target carries no input transformation, so what a
    // consumer reads is the whole envelope.
    let rows = rows();
    let mut queues = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let (url, queue_arn) = started.create_queue(&format!("fan-out-sink-{index}"));
        let rule = format!("fan-out-rule-{index}");
        let created = started.events(
            "PutRule",
            &json!({ "Name": rule, "EventPattern": row.pattern.to_string() }),
        );
        let rule_arn = created["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("expected a RuleArn for {rule:?}, got {created}"))
            .to_string();
        let put = started.events(
            "PutTargets",
            &json!({ "Rule": rule, "Targets": [{ "Id": "sink", "Arn": queue_arn }] }),
        );
        assert_eq!(
            put["FailedEntryCount"],
            json!(0),
            "case {:?}: the target must be installed, got {put}",
            row.case
        );
        started.admitting_rule(&url, &queue_arn, &rule_arn);
        queues.push(url);
    }

    // The control write first: the queue it acts on is one no rule delivers to, so what the
    // control rule receives is the record of a call the account really served.
    let (control_url, _) = started.create_queue("fan-out-control");
    started.sqs(
        "TagQueue",
        &json!({ "QueueUrl": control_url, "Tags": { "owner": "api-call-events" } }),
    );

    // Then the two control-plane writes of the service with no bus events of its own.
    started.dynamodb(
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    );
    let active = started.table_status("ACTIVE");
    assert_eq!(
        active, "ACTIVE",
        "the table must reach ACTIVE, got {active:?}; the log:\n{}",
        started.log()
    );
    started.dynamodb("DeleteTable", &json!({ "TableName": TABLE }));

    for ((row, url), index) in rows.iter().zip(queues.iter()).zip(0..) {
        let case = row.case;
        let (source, event_name) = row.expects;
        let delivery = started.delivered(url, 1);
        assert_eq!(
            delivery.len(),
            1,
            "case {case:?}: expected exactly one delivered envelope on rule {index}, \
             got {delivery:?}; the log:\n{}",
            started.log()
        );
        let envelope = &delivery[0];
        assert_eq!(
            envelope["source"],
            json!(source),
            "case {case:?}: expected source {source:?}, got {envelope}"
        );
        assert_eq!(
            envelope["detail-type"],
            json!(API_CALL_DETAIL_TYPE),
            "case {case:?}: expected detail-type {API_CALL_DETAIL_TYPE:?}, got {envelope}"
        );
        assert_eq!(
            envelope["account"],
            json!(ACCOUNT),
            "case {case:?}: expected account {ACCOUNT:?}, got {envelope}"
        );
        assert_eq!(
            envelope["region"],
            json!(REGION),
            "case {case:?}: expected region {REGION:?}, got {envelope}"
        );
        let detail = &envelope["detail"];
        assert_eq!(
            detail["eventName"],
            json!(event_name),
            "case {case:?}: expected detail.eventName {event_name:?}, got {detail}"
        );
        assert_eq!(
            detail["awsRegion"],
            json!(REGION),
            "case {case:?}: expected detail.awsRegion {REGION:?}, got {detail}"
        );
        assert_eq!(
            detail["readOnly"],
            json!(false),
            "case {case:?}: a write must be recorded readOnly=false, got {detail}"
        );
        assert_eq!(
            detail["eventCategory"],
            json!("Management"),
            "case {case:?}: expected a management record, got {detail}"
        );
        let addressed = if source == SQS_EVENT_SOURCE {
            ("queueUrl", control_url.clone())
        } else {
            ("tableName", TABLE.to_string())
        };
        assert_eq!(
            detail["requestParameters"][addressed.0],
            json!(addressed.1),
            "case {case:?}: the record must carry the {} it was dispatched with, got {detail}",
            addressed.0
        );
    }
}
