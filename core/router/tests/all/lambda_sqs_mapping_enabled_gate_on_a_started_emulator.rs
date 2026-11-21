// SPDX-License-Identifier: CC0-1.0

//! `Enabled` as the DELIVERY GATE of an Amazon SQS event source mapping, driven against the
//! emulator a customer STARTS: the built binary, on its own port, with request authentication
//! on, reached over TLS with SigV4-signed calls.
//!
//! The rule pinned here: **"when true, the event source mapping is active. When false, Lambda
//! pauses polling and invocation" (`CreateEventSourceMapping$Enabled`, which
//! `UpdateEventSourceMapping` carries too) — so a mapping reporting `Enabled` over a queue
//! receives from it and invokes its function for the messages waiting there, deleting each
//! batch the invocation processed; the SAME mapping moved to `Disabled` takes nothing out of
//! the queue, so a message sent meanwhile stays WAITING on it and nothing is invoked for it;
//! and enabled again, it delivers exactly that withheld message — and that holds on the
//! emulator as it is STARTED, not only on a router a test assembles in its own process.**
//!
//! Four crates have to agree for that to hold, so the suite lives here: Amazon SQS holds the
//! queue and the messages on it, IAM answers for the execution role Lambda reads the queue as
//! and the handler writes as, CloudFormation archives the inline handler into the function's
//! deployment package, and Lambda polls and RUNS the handler — whose own effect lands in
//! Amazon S3, where this suite reads it. The owning crate's
//! `event_source_mapping_pause_and_resume_delivery` pins the same gate on a router assembled
//! in the test process with authentication off; the emulator a client runs is a different
//! configuration of the same code (the credentials are verified, each call arrives over its own
//! TLS connection, and the poller, the handler and the handler's own SDK call all run on the
//! binary's own multi-thread runtime while the client is reading S3), so this suite is the one
//! that pins the gate THERE. ONE wiring is driven, since starting the binary costs what
//! starting the emulator costs.
//!
//! Every observation is one a client makes — `ListObjectsV2` and `GetObject` on the bucket the
//! handler writes, `GetQueueAttributes` and `ReceiveMessage` on the source queue, and Lambda's
//! own `GetEventSourceMapping` — and every wait is on a CONDITION with a generous deadline, so
//! a loaded host delays a leg instead of weakening it. The absence leg looks again over a quiet
//! window instead of trusting one look: nothing is polling the queue then, so a delivery that
//! was going to be made has every chance to show up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The Region every call below is signed for, and so the Region the bucket, the queue, the
/// role and the function live in.
const REGION: &str = "us-east-1";

/// The `StackStatus` the consumer's deployment must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long a start may take to publish its files, how long a leg's condition is waited for,
/// and how often this suite looks while waiting. Every wait is on a CONDITION; the spans are
/// generous, so a loaded host spends none of them.
const STARTUP_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long the absence leg keeps LOOKING once the mapping reports `Disabled` and the message
/// is waiting on the queue: well past the span the delivered leg took to leave its report, so a
/// delivery the rule forbids has every chance to appear.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

/// Ends an emulator a crashed test left behind, as `tests/emulator_core` does.
const ORPHAN_IDLE_TIMEOUT_SECS: &str = "180";

/// The stack the execution role and the consuming function are deployed by, and the names the
/// bucket and the queue are created under.
const STACK: &str = "started-emulator-queue-mapping";

/// The key prefix the handler writes every record it is handed under.
const REPORT_PREFIX: &str = "records/";

/// The `State` a mapping polls its event source in, and the state it is paused in
/// (`EventSourceMappingConfiguration$State`).
const STATE_ENABLED: &str = "Enabled";
/// See [`STATE_ENABLED`].
const STATE_DISABLED: &str = "Disabled";

/// The attributes a client reads the queue's own view of its messages from.
const VISIBLE_MESSAGES: &str = "ApproximateNumberOfMessages";
/// See [`VISIBLE_MESSAGES`].
const IN_FLIGHT_MESSAGES: &str = "ApproximateNumberOfMessagesNotVisible";

/// The body delivered while the mapping polls, and the one sent while it is paused.
const WARM_UP: &str = "delivered-while-enabled";
/// See [`WARM_UP`].
const WITHHELD: &str = "sent-while-paused";

/// The handler the stack deploys: for each record of the event it is handed it writes ONE
/// object carrying that record's body, keyed by the record's own `messageId` and the delivery
/// count the queue reports for it — so an invocation that happened leaves an object naming the
/// message it was handed, and one that did not leaves nothing.
const RECORDING_HANDLER: &str = r#"import json
import os

import boto3


def handler(event, context):
    client = boto3.client("s3")
    bucket = os.environ["REPORT_BUCKET"]
    for record in event["Records"]:
        attempt = record["attributes"]["ApproximateReceiveCount"]
        report = {"body": record["body"], "attempt": attempt}
        client.put_object(
            Bucket=bucket,
            Key="records/" + record["messageId"] + "-" + attempt,
            Body=json.dumps(report).encode("utf-8"),
        )
    return {"batchItemFailures": []}
"#;

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
    /// Start the built binary on a free port in a home folder of its own, and wait until it has
    /// published the files a client reads its port, its certificate and its credentials from.
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

    /// Everything the emulator has written to its own log, which is where a delivery that could
    /// not be made, a throttled run or a refused call says so.
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

    /// One CloudFormation call over its awsQuery wire.
    fn cloudformation(&self, action: &str, members: &[(&str, &str)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version=2010-05-15");
        for (key, value) in members {
            body.push('&');
            body.push_str(&form_encode(key));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        self.call(
            "cloudformation",
            "POST",
            "/",
            &[("content-type", "application/x-www-form-urlencoded")],
            body.into_bytes(),
        )
    }

    /// One Amazon SQS call, which must answer 200.
    fn sqs(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, text) = self.call(
            "sqs",
            "POST",
            "/",
            &[
                ("content-type", "application/x-amz-json-1.0"),
                ("x-amz-target", &format!("AmazonSQS.{operation}")),
            ],
            body.to_string().into_bytes(),
        );
        assert_eq!(
            status, 200,
            "case {case:?}: Amazon SQS {operation} must answer 200, got {status} with {text}; \
             the log:\n{}",
            self.log()
        );
        parsed(&text)
    }

    /// One Lambda `restJson1` call, as its status and parsed body.
    fn lambda(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let payload = body.map(Value::to_string).unwrap_or_default();
        let (status, text) = self.call(
            "lambda",
            method,
            path,
            &[("content-type", "application/json")],
            payload.into_bytes(),
        );
        (status, parsed(&text))
    }

    /// One request against Amazon S3's REST surface, path-style.
    fn s3(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        self.call(
            "s3",
            method,
            path,
            &[("content-type", "application/octet-stream")],
            body.as_bytes().to_vec(),
        )
    }
}

/// `text` as JSON, or null for a body that is not JSON at all.
fn parsed(text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// The binary Cargo built for this package, named at COMPILE time rather than looked up among
/// the variables of the running process, as the sibling suites that start the same binary name
/// it.
fn emulator_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aws-emulator"))
}

/// A new folder to serve as an emulator's home: never one another user created first.
fn new_home() -> PathBuf {
    static STARTS: AtomicU32 = AtomicU32::new(0);
    let name = format!("emulator-queue-mapping-{}", std::process::id());
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

/// The folder under `folder` holding a fully written `config`, published last, and not a link.
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

/// The stack this suite deploys: the execution role Lambda reads the queue as and the handler
/// writes its reports as, and the recording function running as it. The inline source is
/// archived into the deployment package by CloudFormation, which is how a function whose
/// handler is written here comes to exist at all.
///
/// The grant names the three calls Lambda makes to consume a queue on the function's behalf —
/// the set AWS's own `AWSLambdaSQSQueueExecutionRole` grants — the log writes an execution role
/// carries, and the ONE write the handler itself makes, scoped to the objects of the bucket it
/// reports into. Nothing else: a delivery that needed any further grant would not be made here.
fn wiring(bucket: &str) -> Value {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ConsumerRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "consume-the-queue-and-report",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [
                                {
                                    "Effect": "Allow",
                                    "Action": [
                                        "sqs:ReceiveMessage",
                                        "sqs:DeleteMessage",
                                        "sqs:GetQueueAttributes",
                                        "logs:CreateLogGroup",
                                        "logs:CreateLogStream",
                                        "logs:PutLogEvents",
                                    ],
                                    "Resource": "*",
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": ["s3:PutObject"],
                                    "Resource": format!("arn:aws:s3:::{bucket}/*"),
                                },
                            ],
                        },
                    }],
                },
            },
            "Consumer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["ConsumerRole", "Arn"] },
                    "Timeout": 10,
                    "Environment": { "Variables": { "REPORT_BUCKET": bucket } },
                    "Code": { "ZipFile": RECORDING_HANDLER },
                },
            },
        },
        "Outputs": { "FunctionName": { "Value": { "Ref": "Consumer" } } },
    })
}

/// Create `STACK` from `template`, wait for a terminal status and answer its outputs. A stack
/// that did not complete is quoted with the emulator's own log.
fn deployed(emulator: &Started, case: &str, template: &str) -> BTreeMap<String, String> {
    let (status, body) = emulator.cloudformation(
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    );
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must be accepted, got {status} with {body}; the log:\n{}",
        emulator.log()
    );
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut described = String::new();
    let mut reported = String::new();
    while Instant::now() < giving_up_at {
        let (status, body) = emulator.cloudformation("DescribeStacks", &[("StackName", STACK)]);
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks must answer 200, got {status} with {body}"
        );
        reported = opt_element(&body, "StackStatus").unwrap_or_default();
        described = body;
        let settled = reported == CREATE_COMPLETE
            || reported.ends_with("_FAILED")
            || reported.contains("ROLLBACK");
        if settled {
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: the stack must reach {CREATE_COMPLETE}, got {reported:?} with \
         {described}; the log:\n{}",
        emulator.log()
    );
    outputs_of(&described)
}

/// Every report the handler has written into `bucket`, keyed by the object key it wrote it
/// under, read as the document the handler put there.
fn reports(emulator: &Started, case: &str, bucket: &str) -> BTreeMap<String, Value> {
    let (status, listed) = emulator.s3(
        "GET",
        &format!("/{bucket}?list-type=2&prefix={REPORT_PREFIX}"),
        "",
    );
    assert_eq!(
        status, 200,
        "case {case:?}: ListObjectsV2({bucket:?}) must be served, got {status} with {listed}"
    );
    let mut found = BTreeMap::new();
    for key in elements(&listed, "Key") {
        let (status, body) = emulator.s3("GET", &format!("/{bucket}/{key}"), "");
        assert_eq!(
            status, 200,
            "case {case:?}: GetObject({bucket:?}, {key:?}) must be served, got {status} with \
             {body}"
        );
        found.insert(key, parsed(&body));
    }
    found
}

/// The bodies the handler reported, in ascending key order: what the invocations it made were
/// handed.
fn reported_bodies(found: &BTreeMap<String, Value>) -> Vec<String> {
    found
        .values()
        .map(|report| report["body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Look at `bucket` until the handler has reported `body`, and answer every body it reported.
fn awaited_report(emulator: &Started, case: &str, bucket: &str, body: &str) -> Vec<String> {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let seen = reported_bodies(&reports(emulator, case, bucket));
        if seen.iter().any(|reported| reported == body) {
            return seen;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the mapping to hand its function {body:?}, it reported \
             {seen:?}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// One attribute of the queue `url` addresses, as `GetQueueAttributes` reports it.
fn queue_attribute(emulator: &Started, case: &str, url: &str, name: &str) -> String {
    let read = emulator.sqs(
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": [name] }),
    );
    read["Attributes"][name]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue reports no {name}: {read}"))
        .to_string()
}

/// How many of the queue's messages a consumer may be handed right now, and how many are in
/// flight with one.
fn queue_counts(emulator: &Started, case: &str, url: &str) -> (String, String) {
    (
        queue_attribute(emulator, case, url, VISIBLE_MESSAGES),
        queue_attribute(emulator, case, url, IN_FLIGHT_MESSAGES),
    )
}

/// Wait until the mapping `uuid` names reports `state`, which is what the `Enabled` setting it
/// was given asks for and the CLOCK owes.
fn await_state(emulator: &Started, case: &str, uuid: &str, state: &str) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = emulator.lambda(
            "GET",
            &format!("/2015-03-31/event-source-mappings/{uuid}"),
            None,
        );
        if status == 200 && read["State"].as_str() == Some(state) {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the mapping {uuid} never reported {state}, last read {status} \
             {read}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// `UpdateEventSourceMapping` setting `Enabled`, waited out to the state that setting asks for.
fn set_enabled(emulator: &Started, case: &str, uuid: &str, enabled: bool) {
    let (status, updated) = emulator.lambda(
        "PUT",
        &format!("/2015-03-31/event-source-mappings/{uuid}"),
        Some(&json!({ "Enabled": enabled })),
    );
    assert_eq!(
        status, 202,
        "case {case:?}: UpdateEventSourceMapping(Enabled={enabled}) must answer 202, got \
         {status} with {updated}; the log:\n{}",
        emulator.log()
    );
    let wanted = if enabled { STATE_ENABLED } else { STATE_DISABLED };
    await_state(emulator, case, uuid, wanted);
}

/// RULE: on the emulator a client STARTS, a queue mapping reporting `Enabled` consumes its
/// queue and its function runs; the same mapping `Disabled` takes nothing, so a message sent
/// then waits on the queue with nothing invoked for it; and enabled again it delivers exactly
/// that message.
#[test]
fn a_queue_mappings_enabled_setting_gates_delivery_on_the_emulator_a_client_starts() {
    let case = "a queue mapping paused by an update and enabled again";
    let emulator = Started::start();
    let bucket = format!("{STACK}-reports");

    let (status, body) = emulator.s3("PUT", &format!("/{bucket}"), "");
    assert_eq!(
        status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must be served, got {status} with {body}"
    );
    let created = emulator.sqs(
        case,
        "CreateQueue",
        &json!({
            "QueueName": format!("{STACK}-source"),
            "Attributes": { "VisibilityTimeout": "30" },
        }),
    );
    let queue_url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue reports no url: {created}"))
        .to_string();
    let queue_arn = queue_attribute(&emulator, case, &queue_url, "QueueArn");

    let outputs = deployed(&emulator, case, &wiring(&bucket).to_string());
    let function = outputs.get("FunctionName").cloned().unwrap_or_else(|| {
        panic!("case {case:?}: the stack must output FunctionName, got {outputs:?}")
    });

    let (status, created) = emulator.lambda(
        "POST",
        "/2015-03-31/event-source-mappings",
        Some(&json!({
            "FunctionName": function,
            "EventSourceArn": queue_arn,
            "BatchSize": 1,
            "Enabled": true,
        })),
    );
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping must answer 202, got {status} with {created}; \
         the log:\n{}",
        emulator.log()
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the created mapping reports no UUID: {created}"))
        .to_string();
    await_state(&emulator, case, &uuid, STATE_ENABLED);

    // Enabled: the message sent now reaches the RUNNING handler, which reports it, and the
    // batch the invocation processed is deleted from the queue.
    emulator.sqs(
        case,
        "SendMessage",
        &json!({ "QueueUrl": &queue_url, "MessageBody": WARM_UP }),
    );
    let delivered = awaited_report(&emulator, case, &bucket, WARM_UP);
    assert_eq!(
        delivered,
        vec![WARM_UP.to_string()],
        "case {case:?}: the polling mapping must hand its function exactly the message it was \
         sent, it reported {delivered:?}; the log:\n{}",
        emulator.log()
    );

    set_enabled(&emulator, case, &uuid, false);

    // Disabled: the message sent now is WAITING on the queue — no consumer took it, so it is
    // neither in flight nor gone — and nothing is reported for it however long this looks.
    emulator.sqs(
        case,
        "SendMessage",
        &json!({ "QueueUrl": &queue_url, "MessageBody": WITHHELD }),
    );
    let quiet_until = Instant::now() + QUIET_WINDOW;
    loop {
        let counts = queue_counts(&emulator, case, &queue_url);
        assert_eq!(
            counts,
            ("1".to_string(), "0".to_string()),
            "case {case:?}: a paused mapping polls nothing, so the message must be waiting on \
             the source queue, got {counts:?} as (visible, in flight); the log:\n{}",
            emulator.log()
        );
        let seen = reported_bodies(&reports(&emulator, case, &bucket));
        assert_eq!(
            seen,
            vec![WARM_UP.to_string()],
            "case {case:?}: a paused mapping invokes nothing, so the handler can report nothing \
             further, but it reported {seen:?}; the log:\n{}",
            emulator.log()
        );
        if Instant::now() >= quiet_until {
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }

    // And the message it withheld is one a client can still be handed, which is what makes the
    // absence a withholding rather than a loss.
    let received = emulator.sqs(
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": &queue_url,
            "MaxNumberOfMessages": 10,
            "VisibilityTimeout": 0,
        }),
    );
    let bodies: Vec<String> = received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        bodies,
        vec![WITHHELD.to_string()],
        "case {case:?}: the message sent while the mapping was paused must still be the \
         queue's to hand over, got {bodies:?}"
    );

    // Enabled again: the same mapping polls the message it was withholding and hands it over.
    set_enabled(&emulator, case, &uuid, true);
    let resumed = awaited_report(&emulator, case, &bucket, WITHHELD);
    let reported: std::collections::BTreeSet<String> = resumed.iter().cloned().collect();
    let expected: std::collections::BTreeSet<String> =
        [WARM_UP.to_string(), WITHHELD.to_string()].into_iter().collect();
    assert_eq!(
        reported, expected,
        "case {case:?}: the mapping enabled again must hand its function the withheld message \
         and nothing else, it reported {resumed:?}; the log:\n{}",
        emulator.log()
    );
}
