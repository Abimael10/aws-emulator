// SPDX-License-Identifier: CC0-1.0

//! A `FilterCriteria`-filtered DynamoDB-stream event source mapping, driven against the
//! emulator a customer STARTS: the built binary, on its own port, with request authentication
//! on, reached over TLS with SigV4-signed calls.
//!
//! The rule under test: **of the records a DynamoDB-stream event source mapping polls, exactly
//! those matching at least one of its `FilterCriteria` patterns are handed to its function — a
//! record no pattern matches is discarded by the poller without an invocation while the
//! mapping's position still advances past it, so the records behind it are delivered — and
//! that holds on the emulator as it is STARTED, not only on a router a test assembles in its
//! own process.** `FilterCriteria` is "an object that defines the filter criteria that
//! determine whether Lambda should process an event" and `FilterCriteria$Filters` is "a list of
//! filters", so the decision is taken per record, before any invocation is made.
//!
//! The sibling suites drive a router built inside the test process, with authentication off, on
//! a runtime the test chose: `all/ddb_stream_lambda_esm_filter_criteria` spans what the rule
//! generalises over there, and the owning crate's own
//! `dynamodb_stream_event_source_filter_criteria` spans it over Lambda, DynamoDB, DynamoDB
//! Streams, Amazon SQS and IAM alone. The emulator a client runs is a different configuration
//! of the same code: the credentials are verified, each call arrives over its own TLS
//! connection, the poller reads and the handler runs on the binary's own multi-thread runtime
//! while the client is long-polling the sink queue, and one process serves every call. This
//! suite is the one that pins the filtered mapping THERE, so a filtered trigger that works
//! in-process and not on the started emulator is a failure a test reports rather than one only
//! a client sees. ONE wiring is driven here, since starting the binary costs what starting the
//! emulator costs.
//!
//! The verdict is read off what the HANDLER forwarded: a record that reached it appears as one
//! message on the sink queue, so a record withheld from it cannot appear. The record this
//! wiring expects delivered LAST is written after the one it expects withheld, and a shard is
//! read in order — so once that record's message is on the queue the withheld one has been read
//! past, and its absence is a verdict rather than a race. Every assertion quotes the emulator's
//! own log, which is where a delivery that could not be made says so.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every call below is signed for, and so the Region the table, its stream, the
/// function and the queue belong to. Not the Region the sibling suites use: the stream's ARN
/// names the Region the mapping's poller reaches DynamoDB Streams and Lambda in, so driving the
/// whole wiring in another Region is what shows the poller reads the Region of the source it was
/// given rather than any default.
const REGION: &str = "us-west-2";

/// The `StackStatus` the deployment must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long a start may take to publish its files, how long the deployment and the delivery are
/// waited for, and how often this suite looks while waiting. Every wait is on a CONDITION; the
/// spans are generous, so a loaded host spends none of them.
const STARTUP_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// Ends an emulator a crashed test left behind, as `tests/emulator_core` does.
const ORPHAN_IDLE_TIMEOUT_SECS: &str = "120";

/// The stack the execution role and the forwarding function are deployed by.
const STACK: &str = "started-emulator-filtered-stream";

/// The attribute every item is keyed by, and the attribute a write changes.
const KEY: &str = "pk";
const VALUE: &str = "v";

/// The `OperationType` a stream record reports for each kind of change
/// (`smithy_query(query="shape", shape="OperationType")`, DynamoDB Streams).
const INSERT: &str = "INSERT";
/// See [`INSERT`].
const MODIFY: &str = "MODIFY";

/// The one pattern the mapping filters with: insert-only, the shape a client wires a projection
/// builder with.
const PATTERN: &str = r#"{"eventName":["INSERT"]}"#;

/// The keys written: one item changed twice, and one written afterwards, whose own record is
/// what proves the change in between was read PAST rather than merely late.
const FIRST_KEY: &str = "row-1";
const SECOND_KEY: &str = "row-2";

/// How long the client waits on one `ReceiveMessage`, in seconds — "The duration (in seconds)
/// for which the call waits for a message to arrive in the queue before returning"
/// (`ReceiveMessageRequest$WaitTimeSeconds`). A long poll is how a client waits for a delivery it
/// cannot otherwise be told about, and it is the wait the recorded wiring makes, so this suite
/// waits on the MESSAGE rather than on the clock.
const WAIT_TIME_SECONDS: u64 = 20;

/// The handler the stack deploys: for each record it is handed it forwards ONE message to the
/// queue its environment names, carrying the change's own name and the changed item's key, both
/// taken out of the record itself — so a message can only appear for a record that reached it.
const FORWARDING_HANDLER: &str = r#"import json
import os

import boto3


def handler(event, context):
    client = boto3.client("sqs")
    forwarded = 0
    for record in event["Records"]:
        client.send_message(
            QueueUrl=os.environ["QUEUE_URL"],
            MessageBody=json.dumps(
                {
                    "eventName": record["eventName"],
                    "pk": record["dynamodb"]["Keys"]["pk"]["S"],
                }
            ),
        )
        forwarded = forwarded + 1
    return {"forwarded": forwarded}
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

    /// One `X-Amz-Target` RPC call, parsed as JSON.
    fn rpc(&self, signing_name: &str, target: &str, body: &Value) -> (u16, Value) {
        let (status, text) = self.call(
            signing_name,
            "POST",
            "/",
            &[
                ("content-type", "application/x-amz-json-1.0"),
                ("x-amz-target", target),
            ],
            body.to_string().into_bytes(),
        );
        (status, parsed(&text))
    }

    /// One DynamoDB call, which must answer 200.
    fn ddb(&self, operation: &str, body: &Value) -> Value {
        let (status, answered) = self.rpc(
            "dynamodb",
            &format!("DynamoDB_20120810.{operation}"),
            body,
        );
        assert_eq!(
            status, 200,
            "DynamoDB {operation} must answer 200, got {status} with {answered}; the log:\n{}",
            self.log()
        );
        answered
    }

    /// One Amazon SQS call, which must answer 200.
    fn sqs(&self, operation: &str, body: &Value) -> Value {
        let (status, answered) = self.rpc("sqs", &format!("AmazonSQS.{operation}"), body);
        assert_eq!(
            status, 200,
            "Amazon SQS {operation} must answer 200, got {status} with {answered}; the log:\n{}",
            self.log()
        );
        answered
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
}

/// `text` as JSON, or null for a body that is not JSON at all.
fn parsed(text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// The binary Cargo built for this package, named at COMPILE time rather than looked up among
/// the variables of the running process, as the sibling suite that starts the same binary names
/// it.
fn emulator_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aws-emulator"))
}

/// A new folder to serve as an emulator's home: never one another user created first.
fn new_home() -> PathBuf {
    static STARTS: AtomicU32 = AtomicU32::new(0);
    let name = format!("emulator-filtered-stream-{}", std::process::id());
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

/// The stack this suite deploys: the execution role the poller reads the stream as and the
/// handler's own send is judged against, and the forwarding function running as it. The inline
/// source is archived into the deployment package by CloudFormation, which is how a function
/// whose handler is written here comes to exist at all.
///
/// The actions a DynamoDB-stream poller is decided on are the stream's own three
/// (`DescribeStream`, `GetShardIterator`, `GetRecords`), with the account-wide `ListStreams`
/// beside them as AWS's own managed execution-role policy for that source
/// (`AWSLambdaDynamoDBExecutionRole`) writes it. Beside them stands the ONE action the handler
/// itself takes and nothing else — no log actions among them — so this wiring is the
/// least-privilege one a client writes for it, and a delivery that needed any further grant
/// would not be made here.
fn wiring(queue_url: &str) -> Value {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "HandlerRole": {
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
                        "PolicyName": "consume-and-forward",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [
                                {
                                    "Effect": "Allow",
                                    "Action": [
                                        "dynamodb:DescribeStream",
                                        "dynamodb:GetShardIterator",
                                        "dynamodb:GetRecords",
                                        "dynamodb:ListStreams",
                                    ],
                                    "Resource": "*",
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": ["sqs:SendMessage"],
                                    "Resource": "*",
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
                    "Role": { "Fn::GetAtt": ["HandlerRole", "Arn"] },
                    "Timeout": 30,
                    "Environment": { "Variables": { "QUEUE_URL": queue_url } },
                    "Code": { "ZipFile": FORWARDING_HANDLER },
                },
            },
        },
        "Outputs": {
            "FunctionName": { "Value": { "Ref": "Consumer" } },
        },
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

/// The stream-enabled source table, waited out to `ACTIVE`, answered as the ARN of the stream it
/// minted.
fn given_table(emulator: &Started, case: &str, table: &str) -> String {
    emulator.ddb(
        "CreateTable",
        &json!({
            "TableName": table,
            "AttributeDefinitions": [{"AttributeName": KEY, "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": KEY, "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
            "StreamSpecification": {
                "StreamEnabled": true,
                "StreamViewType": "NEW_AND_OLD_IMAGES",
            },
        }),
    );
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = emulator.ddb("DescribeTable", &json!({"TableName": table}));
        let active = described["Table"]["TableStatus"] == json!("ACTIVE");
        if let (true, Some(arn)) = (active, described["Table"]["LatestStreamArn"].as_str()) {
            return arn.to_string();
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the table {table:?} never reached ACTIVE with a stream, last read \
             {described}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// The filtered mapping over `stream_arn`, polled until Lambda reports it `Enabled`.
fn given_mapping(emulator: &Started, case: &str, function: &str, stream_arn: &str) {
    let (status, created) = emulator.lambda(
        "POST",
        "/2015-03-31/event-source-mappings",
        Some(&json!({
            "FunctionName": function,
            "EventSourceArn": stream_arn,
            "StartingPosition": "TRIM_HORIZON",
            "BatchSize": 10,
            "MaximumBatchingWindowInSeconds": 1,
            "FilterCriteria": { "Filters": [{ "Pattern": PATTERN }] },
        })),
    );
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping must answer 202, got {status} with {created}; \
         the log:\n{}",
        emulator.log()
    );
    assert_eq!(
        created["FilterCriteria"]["Filters"][0]["Pattern"].as_str(),
        Some(PATTERN),
        "case {case:?}: the mapping must report the criteria it was created with, got {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the mapping reports no UUID, got {created}"))
        .to_string();
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = emulator.lambda(
            "GET",
            &format!("/2015-03-31/event-source-mappings/{uuid}"),
            None,
        );
        if status == 200 && read["State"].as_str() == Some("Enabled") {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the mapping {uuid} never reported Enabled, last read {status} {read}; \
             the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// How this suite names one delivered record: the change it reported and the key it changed.
fn marker(event_name: &str, key: &str) -> String {
    format!("{event_name}#{key}")
}

/// Long-poll `url` until every marker of `expected` has arrived, answering every marker seen in
/// the order they arrived — each message deleted, so a later look finds only later ones.
fn awaited_markers(
    emulator: &Started,
    case: &str,
    url: &str,
    expected: &[String],
) -> Vec<String> {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let received = emulator.sqs(
            "ReceiveMessage",
            &json!({
                "QueueUrl": url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": WAIT_TIME_SECONDS,
            }),
        );
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default().to_string();
            emulator.sqs(
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            );
            let forwarded = parsed(&body);
            seen.push(marker(
                forwarded["eventName"].as_str().unwrap_or_default(),
                forwarded["pk"].as_str().unwrap_or_default(),
            ));
        }
        if expected.iter().all(|marker| seen.contains(marker)) {
            return seen;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the handler to be handed {expected:?}, it forwarded {seen:?}; \
             the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// RULE: on the emulator a client starts, a DynamoDB-stream mapping carrying `FilterCriteria`
/// hands its function exactly the records a pattern matches, and the records behind one it
/// rejected are still delivered.
#[test]
fn a_filtered_stream_mapping_delivers_its_matches_on_the_emulator_a_client_starts() {
    let case = "an insert-only filtered mapping on a started emulator";
    let emulator = Started::start();
    let table = format!("{STACK}-source");

    let created = emulator.sqs("CreateQueue", &json!({"QueueName": format!("{STACK}-sink")}));
    let queue_url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue reports no url, got {created}"))
        .to_string();

    let outputs = deployed(&emulator, case, &wiring(&queue_url).to_string());
    let function = outputs.get("FunctionName").cloned().unwrap_or_else(|| {
        panic!("case {case:?}: the stack must output FunctionName, got {outputs:?}")
    });
    let stream_arn = given_table(&emulator, case, &table);

    // Every write is made BEFORE the mapping exists: a mapping reading from the oldest retained
    // record reads the backlog, so the filter decides records already in the shard. The change
    // to the first item is made before the second item is written, so the second item's record
    // arriving proves the change was read past.
    for (key, value) in [
        (FIRST_KEY, "first"),
        (FIRST_KEY, "second"),
        (SECOND_KEY, "first"),
    ] {
        emulator.ddb(
            "PutItem",
            &json!({
                "TableName": &table,
                "Item": {KEY: {"S": key}, VALUE: {"S": value}},
            }),
        );
    }
    given_mapping(&emulator, case, &function, &stream_arn);

    let expected = vec![marker(INSERT, FIRST_KEY), marker(INSERT, SECOND_KEY)];
    let seen = awaited_markers(&emulator, case, &queue_url, &expected);
    let withheld = marker(MODIFY, FIRST_KEY);
    assert!(
        !seen.contains(&withheld),
        "case {case:?}: {PATTERN} matches no {MODIFY} record, so no invocation may be handed the \
         change made to {FIRST_KEY:?}, but the handler forwarded {seen:?}; the log:\n{}",
        emulator.log()
    );
}
