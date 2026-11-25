// SPDX-License-Identifier: CC0-1.0

//! The management-event fan-out of a GLOBAL service, driven against the emulator a customer
//! STARTS: the built binary, on its own port, with request authentication on, reached over
//! TLS with SigV4-signed calls.
//!
//! RULE: while a trail of the account is logging management events of a call's read/write
//! class, every control-plane call the account serves is ALSO published onto the DEFAULT
//! event bus of the Region the record is filed in — the partition's leader Region for a
//! GLOBAL service, whichever Region the call was signed for — with `source`
//! `aws.<endpoint-prefix>` of the serving service, `detail-type`
//! `AWS API Call via CloudTrail` and the CloudTrail record of the call as `detail`; a rule of
//! that Region's bus whose pattern names those fields is handed one event per matching call
//! and delivers it to its target, while the same rule on any other Region's bus is handed
//! none of them.
//!
//! The sibling suite `all/cloudtrail_api_call_events_by_region` spans what the rule
//! generalises over on a router built inside the test process, with authentication off, and
//! `all/cloudtrail_api_call_events_on_a_started_emulator` pins the REGIONAL half of it on the
//! started binary. This suite is the GLOBAL half there: one rule pinned to several event
//! names of one global service, a twin of that rule in a second Region, and the three writes
//! those names cover — so a channel that delivers a global service's records in-process and
//! not on the started emulator is a failure a test reports rather than one only a client
//! sees.
//!
//! Every assertion quotes the emulator's own log, which is where a delivery that could not be
//! made says so.
//!
//! The start-and-sign harness below is this module's own, as it is in each of the sibling
//! suites that start the same binary: each of those holds its copy private to its own module,
//! so there is nothing a third suite can reach for.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The partition's leader Region: where a GLOBAL service's management events are filed, so
/// the Region whose default bus carries them and where the trail of this suite is created.
const LEADER_REGION: &str = "us-east-1";

/// A second Region, which is where the twin rule lives: the "comparison Region" whose bus
/// must see none of a global service's events.
const OTHER_REGION: &str = "us-west-2";

/// The account every emulated resource belongs to (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// IAM's API version, which every awsQuery form body of it carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The `detail-type` CloudTrail's management-event fan-out carries
/// (`service_common::api_call_events::API_CALL_DETAIL_TYPE`).
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";

/// The `source` an event published for an IAM call carries, and the `eventSource` the record
/// inside it names.
const IAM_EVENT_SOURCE: &str = "aws.iam";
const IAM_RECORD_SOURCE: &str = "iam.amazonaws.com";

/// The writes this suite makes, in the order it makes them: the `eventName` each is recorded
/// under, which is what the one rule's pattern pins.
const WRITE_EVENT_NAMES: [&str; 3] = ["CreateRole", "TagRole", "DeleteRole"];

/// The role those writes create, tag and delete.
const ROLE: &str = "started-emulator-global-fan-out";

/// The trail and the bucket it delivers into.
const TRAIL: &str = "started-emulator-global-fan-out";
const BUCKET: &str = "started-emulator-global-fan-out-trail";

/// The rule name both Regions' rules carry, and the queue name both Regions' queues carry: a
/// rule and a queue are REGIONAL resources, so the twin is the same pair of names in another
/// Region.
const RULE: &str = "global-fan-out-rule";
const QUEUE: &str = "global-fan-out-sink";

/// The input transformation the targets deliver through — the shape a client wires an audit
/// consumer with: the rule that matched, and the whole event beside it. The two placeholders
/// are EventBridge's own reserved fills.
const INPUT_TEMPLATE: &str =
    r#"{"rule": "<aws.events.rule-name>", "event": <aws.events.event.json>}"#;

/// The trust policy the role is created with, written so it carries nothing an
/// `application/x-www-form-urlencoded` body treats specially.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"sts:AssumeRole"}]}"#
);

/// How long a start may take to publish its files, and how long a delivery is waited for.
/// Every wait is on a CONDITION; the spans are generous, so a loaded host spends none of them.
const STARTUP_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// Ends an emulator a crashed test left behind, as `tests/emulator_core` does.
const ORPHAN_IDLE_TIMEOUT_SECS: &str = "120";

/// How long the client waits on one `ReceiveMessage`, in seconds — "The duration (in
/// seconds) for which the call waits for a message to arrive in the queue before returning"
/// (`ReceiveMessageRequest$WaitTimeSeconds`). A long poll is how a consumer of this channel
/// waits for a delivery it cannot otherwise be told about.
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
        started.key =
            profile_value(&credentials, "aws_access_key_id").expect("the published access key id");
        started.secret =
            profile_value(&credentials, "aws_secret_access_key").expect("the published secret");
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

    /// One signed call in `region`, answered with its status and body text.
    fn call(
        &self,
        signing_name: &str,
        region: &str,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> (u16, String) {
        let uri = format!("https://127.0.0.1:{}{path}", self.port);
        let mut request = http::Request::builder().method(method).uri(uri);
        // The authority an SDK pointed at this emulator sends: the endpoint it was given, not
        // one of AWS's own names. It is the authority this emulator reports its own addresses
        // at, so a client that stores one reaches the emulator with it.
        request = request.header("host", format!("127.0.0.1:{}", self.port));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let digest = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&body));
        request = request.header("x-amz-content-sha256", digest);
        let request = request.body(body).expect("a well-formed request");
        let signed = self.signed(request, signing_name, region);
        let response = self
            .client
            .execute(signed.try_into().expect("a request reqwest can send"))
            .expect("the emulator answers");
        let status = response.status().as_u16();
        let body = response.text().unwrap_or_default();
        (status, body)
    }

    /// `request` signed with the published key pair for `signing_name` in `region`, as an SDK
    /// signs it.
    fn signed(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
        region: &str,
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
            .region(region)
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

    /// One `X-Amz-Target` RPC call in `region`, parsed as JSON.
    fn rpc(
        &self,
        signing_name: &str,
        region: &str,
        target: &str,
        json_version: &str,
        body: &Value,
    ) -> (u16, Value) {
        let (status, text) = self.call(
            signing_name,
            region,
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

    /// One Amazon SQS call in `region`, which must answer 200.
    fn sqs(&self, region: &str, operation: &str, body: &Value) -> Value {
        let (status, answered) =
            self.rpc("sqs", region, &format!("AmazonSQS.{operation}"), "1.0", body);
        assert_eq!(
            status, 200,
            "Amazon SQS {operation} in {region} must answer 200, got {status} with \
             {answered}; the log:\n{}",
            self.log()
        );
        answered
    }

    /// One EventBridge call in `region`, which must answer 200.
    fn events(&self, region: &str, operation: &str, body: &Value) -> Value {
        let (status, answered) =
            self.rpc("events", region, &format!("AWSEvents.{operation}"), "1.1", body);
        assert_eq!(
            status, 200,
            "EventBridge {operation} in {region} must answer 200, got {status} with \
             {answered}; the log:\n{}",
            self.log()
        );
        answered
    }

    /// One CloudTrail call, which must answer 200.
    fn cloudtrail(&self, operation: &str, body: &Value) -> Value {
        let (status, answered) = self.rpc(
            "cloudtrail",
            LEADER_REGION,
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

    /// One IAM awsQuery form POST, which must answer 200, as its body text.
    fn iam(&self, action: &str, parameters: &str) -> String {
        let body = format!("Version={IAM_API_VERSION}&Action={action}&{parameters}");
        let (status, text) = self.call(
            "iam",
            LEADER_REGION,
            "POST",
            "/",
            &[("content-type", "application/x-www-form-urlencoded")],
            body.into_bytes(),
        );
        assert_eq!(
            status, 200,
            "IAM {action} must answer 200, got {status} with {text}; the log:\n{}",
            self.log()
        );
        text
    }

    /// The trail's destination bucket, with the policy that lets CloudTrail write into it.
    fn create_bucket(&self) {
        let (status, body) = self.call(
            "s3",
            LEADER_REGION,
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
            LEADER_REGION,
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

    /// Create a queue in `region` and hand back its url and the ARN it reports for itself.
    fn create_queue(&self, region: &str, name: &str) -> (String, String) {
        let created = self.sqs(region, "CreateQueue", &json!({ "QueueName": name }));
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("expected a QueueUrl for {name:?}, got {created}"))
            .to_string();
        let attributes = self.sqs(
            region,
            "GetQueueAttributes",
            &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
        );
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| panic!("expected a QueueArn for {name:?}, got {attributes}"))
            .to_string();
        (url, arn)
    }

    /// Let `rule_arn` — and only it — send to the queue at `url`.
    fn admitting_rule(&self, region: &str, url: &str, queue_arn: &str, rule_arn: &str) {
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
            region,
            "SetQueueAttributes",
            &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
        );
    }

    /// Long-poll `url` in `region` until it has handed back at least `wanted` envelopes, or
    /// [`SETTLE_DEADLINE`] has passed — whichever comes first, and then ONCE more. The
    /// CONDITION is what is waited for; the deadline only bounds a wait that will never be
    /// satisfied.
    ///
    /// The extra look is what lets a row read "exactly these envelopes": one more arriving a
    /// moment after the last awaited one would otherwise go unseen.
    fn delivered(&self, region: &str, url: &str, wanted: usize) -> Vec<Value> {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        let mut seen: Vec<Value> = Vec::new();
        loop {
            let answer = self.sqs(
                region,
                "ReceiveMessage",
                &json!({
                    "QueueUrl": url,
                    "MaxNumberOfMessages": 10,
                    "WaitTimeSeconds": WAIT_TIME_SECONDS,
                }),
            );
            let handed = answer["Messages"].as_array().cloned().unwrap_or_default();
            let enough = seen.len() >= wanted;
            for message in &handed {
                let body = message["Body"].as_str().unwrap_or_default();
                seen.push(serde_json::from_str(body).unwrap_or_else(|error| {
                    panic!("a delivered envelope is JSON ({error}): {body}")
                }));
            }
            // A look that handed something back is never the last one: the next one is what
            // says no further envelope followed it.
            if (enough && handed.is_empty()) || Instant::now() >= deadline {
                return seen;
            }
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

/// The binary Cargo built for this package, named at COMPILE time rather than looked up among
/// the variables of the running process, as the sibling suites that start the same binary name
/// it.
fn emulator_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aws-emulator"))
}

/// A new folder to serve as an emulator's home: never one another user created first.
fn new_home() -> PathBuf {
    static STARTS: AtomicU32 = AtomicU32::new(0);
    let name = format!("emulator-iam-api-call-events-{}", std::process::id());
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

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// The pattern both Regions' rules carry: the envelope of this channel, the serving service's
/// record source, and the event names of the three writes below.
fn rule_pattern() -> Value {
    json!({
        "source": [IAM_EVENT_SOURCE],
        "detail-type": [API_CALL_DETAIL_TYPE],
        "detail": {
            "eventSource": [IAM_RECORD_SOURCE],
            "eventName": WRITE_EVENT_NAMES,
        },
    })
}

/// Install the rule of `region`: a queue, the rule pinned to [`rule_pattern`], a target
/// delivering through [`INPUT_TEMPLATE`], and the grant that lets that rule send to the queue.
fn rule_on(started: &Started, region: &str, queue: &str) -> String {
    let (url, queue_arn) = started.create_queue(region, queue);
    let created = started.events(
        region,
        "PutRule",
        &json!({ "Name": RULE, "EventPattern": rule_pattern().to_string() }),
    );
    let rule_arn = created["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a RuleArn in {region}, got {created}"))
        .to_string();
    let put = started.events(
        region,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "Targets": [{
                "Id": "sink",
                "Arn": queue_arn,
                "InputTransformer": { "InputTemplate": INPUT_TEMPLATE },
            }],
        }),
    );
    assert_eq!(
        put["FailedEntryCount"],
        json!(0),
        "the target in {region} must be installed, got {put}"
    );
    started.admitting_rule(region, &url, &queue_arn, &rule_arn);
    url
}

#[test]
fn a_global_services_api_call_events_reach_the_leader_regions_rule_on_a_started_emulator() {
    let started = Started::start();

    // A multi-Region trail of the account, logging the WRITE management events of every
    // Region and the partition's global services with them.
    started.create_bucket();
    started.cloudtrail(
        "CreateTrail",
        &json!({
            "Name": TRAIL,
            "S3BucketName": BUCKET,
            "IsMultiRegionTrail": true,
            "IncludeGlobalServiceEvents": true,
        }),
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

    // The rule of the leader Region, and its TWIN on the comparison Region's bus: a rule and
    // a queue are regional resources, so the twin carries the same two names in the other
    // Region — which is also what a client deploying one stack per Region writes.
    let leader_url = rule_on(&started, LEADER_REGION, QUEUE);
    let other_url = rule_on(&started, OTHER_REGION, QUEUE);

    // The three writes the rule's pattern names, each a call of the GLOBAL service.
    let created = started.iam(
        "CreateRole",
        &format!("RoleName={ROLE}&AssumeRolePolicyDocument={TRUST_POLICY}"),
    );
    let role_id = xml_element(&created, "RoleId")
        .unwrap_or_else(|| panic!("CreateRole must answer a <RoleId>, got {created}"));
    started.iam(
        "TagRole",
        &format!("RoleName={ROLE}&Tags.member.1.Key=owner&Tags.member.1.Value=audit"),
    );
    started.iam("DeleteRole", &format!("RoleName={ROLE}"));

    // The leader Region's rule is handed one envelope per matching call; the comparison
    // Region's twin is read only once those have arrived, so it is read after the fan-out has
    // run rather than before.
    let arrived = started.delivered(LEADER_REGION, &leader_url, WRITE_EVENT_NAMES.len());
    assert_eq!(
        arrived.len(),
        WRITE_EVENT_NAMES.len(),
        "expected one envelope per matching call ({:?}), got {arrived:?}; the log:\n{}",
        WRITE_EVENT_NAMES,
        started.log()
    );
    let elsewhere = started.delivered(OTHER_REGION, &other_url, 0);
    assert!(
        elsewhere.is_empty(),
        "a global service's events belong to {LEADER_REGION} alone, so the {OTHER_REGION} \
         twin must stay empty, got {elsewhere:?}"
    );

    // Each envelope, read by the event name its record carries: the rows are the three writes,
    // which differ in what the call did and in what its record carries.
    for event_name in WRITE_EVENT_NAMES {
        let found = arrived.iter().find(|delivered| {
            delivered["event"]["detail"]["eventName"] == json!(event_name)
        });
        let delivered = found.unwrap_or_else(|| {
            panic!("case {event_name:?}: no envelope carries this event name, got {arrived:?}")
        });
        assert_eq!(
            delivered["rule"],
            json!(RULE),
            "case {event_name:?}: expected the matched rule {RULE:?}, got {delivered}"
        );
        let envelope = &delivered["event"];
        assert_eq!(
            envelope["source"],
            json!(IAM_EVENT_SOURCE),
            "case {event_name:?}: expected source {IAM_EVENT_SOURCE:?}, got {envelope}"
        );
        assert_eq!(
            envelope["detail-type"],
            json!(API_CALL_DETAIL_TYPE),
            "case {event_name:?}: expected detail-type {API_CALL_DETAIL_TYPE:?}, got {envelope}"
        );
        assert_eq!(
            envelope["account"],
            json!(ACCOUNT),
            "case {event_name:?}: expected account {ACCOUNT:?}, got {envelope}"
        );
        assert_eq!(
            envelope["region"],
            json!(LEADER_REGION),
            "case {event_name:?}: expected region {LEADER_REGION:?}, got {envelope}"
        );
        let detail = &envelope["detail"];
        assert_eq!(
            detail["eventSource"],
            json!(IAM_RECORD_SOURCE),
            "case {event_name:?}: expected eventSource {IAM_RECORD_SOURCE:?}, got {detail}"
        );
        assert_eq!(
            detail["awsRegion"],
            json!(LEADER_REGION),
            "case {event_name:?}: expected awsRegion {LEADER_REGION:?}, got {detail}"
        );
        assert_eq!(
            detail["readOnly"],
            json!(false),
            "case {event_name:?}: a write must be recorded readOnly=false, got {detail}"
        );
        assert_eq!(
            detail["requestParameters"]["roleName"],
            json!(ROLE),
            "case {event_name:?}: the record must carry the role the call named, got {detail}"
        );
    }

    // The create's record carries what its own response answered, which is what makes the
    // envelope the record of that very call rather than a notice that one happened.
    let create = arrived
        .iter()
        .find(|delivered| delivered["event"]["detail"]["eventName"] == json!("CreateRole"))
        .expect("the create's envelope");
    assert_eq!(
        create["event"]["detail"]["responseElements"]["role"]["roleId"],
        json!(role_id),
        "the create's record must carry the id its response answered ({role_id}), got {create}"
    );
}
