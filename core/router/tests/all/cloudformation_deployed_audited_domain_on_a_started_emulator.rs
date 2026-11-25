// SPDX-License-Identifier: CC0-1.0

//! The CloudFormation-deployed, audited Amazon OpenSearch Service domain lifecycle, driven
//! against the emulator a customer STARTS: the built binary, on its own port, with request
//! authentication on, reached over TLS with SigV4-signed calls.
//!
//! RULE: **a domain an accepted create leaves behind becomes a usable cluster — so the owning
//! service reports it `Processing=false` with the `Endpoint` it published, that endpoint
//! serves the signed REST data plane including the security plugin's audit-configuration
//! resource, and every decision the cluster takes about an audited request is delivered to the
//! log group the `AUDIT_LOGS` option names — and that holds on the emulator as it is STARTED,
//! not only on a router a test assembles in its own process.**
//!
//! The sibling suite `all/cloudformation_deployed_audited_domain_lifecycle` spans what the
//! rule generalises over on a router built inside the test process, with authentication off.
//! The emulator a client runs is a different configuration of the same code: the credentials
//! are verified, each call arrives over its own TLS connection, the domain's data plane is
//! addressed by the HOST its endpoint was published as, and one process serves every call.
//! This suite is the one that pins the lifecycle THERE, so a deploy or a delivery that works
//! in-process and not on the started emulator is a failure a test reports rather than one only
//! a client sees. ONE wiring is driven here, since starting the binary costs what starting the
//! emulator costs.
//!
//! Every assertion quotes the emulator's own log, which is where a call it refused or a
//! delivery it could not make says so.
//!
//! The start-and-sign harness below is this module's own, as it is in each of the sibling
//! suites that start the same binary
//! (`all/cloudtrail_api_call_events_on_a_started_emulator`,
//! `all/cfn_deployed_pipeline_on_a_started_emulator`): each of those holds its copy private to
//! its own module, so there is nothing a third suite can reach for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

// What the estate under test IS, and what an audit record of a decision says, come from the
// in-process sibling, which states both once: the two suites drive one lifecycle, so neither
// deploys a template or reads a record of its own.
use crate::cloudformation_deployed_audited_domain_lifecycle::{
    at_layer, enabling_patch, fields_reporting_the_answer, group_name, is_enabled, template,
    AUDIT_LOG_TYPE, CATEGORY_AUTHENTICATED, CATEGORY_GRANTED_PRIVILEGES, GET_REQUEST,
    LEGACY_AUDIT_PATH, LOG_TYPES, MASTER_USER_PARAMETER, POLICY_NAME_PARAMETER, PROBE_STREAMS,
    REGION,
};

/// How long a start may take to publish its files, and how long a condition is waited on.
/// Every wait is on a CONDITION; the spans are generous, so a loaded host spends none of them.
const STARTUP_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// Ends an emulator a crashed test left behind, as `tests/emulator_core` does.
const ORPHAN_IDLE_TIMEOUT_SECS: &str = "120";

/// The stack the estate is deployed as, and the status a completed deployment reports. The
/// name is a long one, as a deployment's own is: the domain's name is CloudFormation's to
/// generate, and it has to fit `DomainName`'s own 28 characters whatever the stack is called.
const STACK: &str = "osds-started-audited-estate-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The resource the audit configuration is enabled through: the spelling a client's own setup
/// step uses.
const AUDIT_PATH: &str = LEGACY_AUDIT_PATH;

/// The index and the documents the audited requests address.
const INDEX: &str = "requests-2026.01";
const DOCUMENT: &str = "seeded";
const ABSENT: &str = "unseeded";

/// One request this suite sends: where it is SENT, what it addresses, and what it carries.
///
/// The authority and the host are two different things here: a service endpoint is reached at
/// the emulator's own front door under the service's AWS host, while a resource's own data
/// plane is reached at the ADDRESS that resource published — which is where a client
/// configured with that address sends its request, and the host it carries.
struct Sent<'r> {
    authority: &'r str,
    host: &'r str,
    signing_name: &'r str,
    method: &'r str,
    path: &'r str,
    headers: &'r [(&'r str, &'r str)],
    body: Vec<u8>,
}

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

    /// Everything the emulator has written to its own log, which is where a call it refused
    /// or a delivery it could not make says so.
    fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("emulator.log")).unwrap_or_default()
    }

    /// The authority of the emulator's own front door, where every service endpoint is
    /// served.
    fn front_door(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// One signed call sent to [`Sent::authority`], answered with its status and body text.
    fn call(&self, sent: Sent<'_>) -> (u16, String) {
        let uri = format!("https://{}{}", sent.authority, sent.path);
        let mut request = http::Request::builder().method(sent.method).uri(uri);
        request = request.header("host", sent.host);
        for (name, value) in sent.headers {
            request = request.header(*name, *value);
        }
        let digest = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&sent.body));
        request = request.header("x-amz-content-sha256", digest);
        let request = request.body(sent.body).expect("a well-formed request");
        let signed = self.signed(request, sent.signing_name);
        let response = self
            .client
            .execute(signed.try_into().expect("a request reqwest can send"))
            .expect("the emulator answers");
        let status = response.status().as_u16();
        let text = response.text().unwrap_or_default();
        (status, text)
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

    /// One awsQuery call of `service`, answered with its status and the XML body.
    fn query(&self, service: &str, version: &str, action: &str, members: &[(&str, &str)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version={version}");
        for (key, value) in members {
            body.push('&');
            body.push_str(&form_encode(key));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        self.call(Sent {
            authority: &self.front_door(),
            host: &format!("{service}.{REGION}.amazonaws.com"),
            signing_name: service,
            method: "POST",
            path: "/",
            headers: &[("content-type", "application/x-www-form-urlencoded")],
            body: body.into_bytes(),
        })
    }

    /// One CloudFormation call over its awsQuery wire.
    fn cloudformation(&self, action: &str, members: &[(&str, &str)]) -> (u16, String) {
        self.query("cloudformation", "2010-05-15", action, members)
    }

    /// One Amazon CloudWatch Logs call over its awsJson1_1 wire.
    fn logs(&self, operation: &str, body: &Value) -> (u16, Value) {
        let (status, text) = self.call(Sent {
            authority: &self.front_door(),
            host: &format!("logs.{REGION}.amazonaws.com"),
            signing_name: "logs",
            method: "POST",
            path: "/",
            headers: &[
                ("content-type", "application/x-amz-json-1.1"),
                ("x-amz-target", &format!("Logs_20140328.{operation}")),
            ],
            body: body.to_string().into_bytes(),
        });
        (status, parsed(&text))
    }

    /// One Amazon OpenSearch Service control-plane call over its `restJson1` wire.
    fn opensearch(&self, method: &str, path: &str) -> (u16, Value) {
        let (status, text) = self.call(Sent {
            authority: &self.front_door(),
            host: &format!("es.{REGION}.amazonaws.com"),
            signing_name: "es",
            method,
            path,
            headers: &[],
            body: Vec::new(),
        });
        (status, parsed(&text))
    }

    /// One signed request to the ADDRESS the domain published, dialled the way a client
    /// configured with that address dials it: the endpoint is the authority the request is
    /// SENT to as well as the host it carries, and the whole of the path is the client's.
    fn at_endpoint(
        &self,
        endpoint: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let payload = body.map(|document| document.to_string().into_bytes());
        let headers: &[(&str, &str)] = if payload.is_some() {
            &[("content-type", "application/json")]
        } else {
            &[]
        };
        let (status, text) = self.call(Sent {
            authority: endpoint,
            host: endpoint,
            signing_name: "es",
            method,
            path,
            headers,
            body: payload.unwrap_or_default(),
        });
        (status, parsed(&text))
    }

    /// The ARN `sts:GetCallerIdentity` reports for the published key pair — the principal the
    /// template names as the domain's master user, which is what the plugin's audit API
    /// admits.
    fn caller_arn(&self) -> String {
        let (status, body) = self.query("sts", "2011-06-15", "GetCallerIdentity", &[]);
        let arn = opt_element(&body, "Arn").unwrap_or_default();
        assert!(
            status == 200 && arn.starts_with("arn:"),
            "setup: GetCallerIdentity must report the signing identity, got {status} with \
             {body}; the log:\n{}",
            self.log()
        );
        arn
    }

    /// The `StackStatus` the stack reports now, with the answer it was read from.
    fn stack_status(&self) -> (String, String) {
        let (status, body) = self.cloudformation("DescribeStacks", &[("StackName", STACK)]);
        assert_eq!(
            status, 200,
            "DescribeStacks must answer 200, got {status} with {body}; the log:\n{}",
            self.log()
        );
        (opt_element(&body, "StackStatus").unwrap_or_default(), body)
    }

    /// Wait until the stack settles in a status that is not still in progress.
    fn settled_stack(&self) -> (String, String) {
        let giving_up_at = Instant::now() + SETTLE_DEADLINE;
        let mut last = self.stack_status();
        while last.0.ends_with("_IN_PROGRESS") && Instant::now() < giving_up_at {
            std::thread::sleep(LOOK_INTERVAL);
            last = self.stack_status();
        }
        last
    }

    /// Every resource failure the stack recorded, so a failing case can say WHY.
    fn failure_reasons(&self) -> Vec<String> {
        let (_, body) = self.cloudformation("DescribeStackEvents", &[("StackName", STACK)]);
        elements(&body, "ResourceStatusReason")
    }

    /// Every record `group` holds, each parsed back from the line it was delivered as, LESS
    /// the events of the two streams the create-time probe opens.
    fn delivered(&self, group: &str) -> Vec<Value> {
        let (status, parsed) = self.logs("FilterLogEvents", &json!({ "logGroupName": group }));
        assert_eq!(
            status, 200,
            "FilterLogEvents({group:?}) must answer 200, got {status} with {parsed}; the \
             log:\n{}",
            self.log()
        );
        parsed["events"]
            .as_array()
            .map(|events| {
                events
                    .iter()
                    .filter(|event| {
                        !PROBE_STREAMS
                            .contains(&event["logStreamName"].as_str().unwrap_or_default())
                    })
                    .map(|event| {
                        let message = event["message"].as_str().unwrap_or_default();
                        serde_json::from_str(message).unwrap_or_else(|_| json!(message))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Wait until `group` holds at least `wanted` records of the work this suite did.
    fn delivered_at_least(&self, group: &str, wanted: usize) -> Vec<Value> {
        let giving_up_at = Instant::now() + SETTLE_DEADLINE;
        let mut records = self.delivered(group);
        while records.len() < wanted && Instant::now() < giving_up_at {
            std::thread::sleep(LOOK_INTERVAL);
            records = self.delivered(group);
        }
        records
    }
}

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
    let name = format!("emulator-audited-domain-{}", std::process::id());
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

/// The log group the deployed domain publishes `log_type` to — the one group this suite reads
/// its records out of, and the three it reads to show they hold none of them.
fn group_of(logical_id: &str) -> String {
    group_name(STACK, logical_id)
}

#[test]
fn a_deployed_audited_domain_is_usable_on_a_started_emulator() {
    let started = Started::start();
    let master_user = started.caller_arn();

    // The estate deploys: four log groups, the grant over them, and the domain that publishes
    // to them.
    let policy_name = format!("{STACK}-delivery");
    let body = template(STACK, None, &LOG_TYPES);
    let (status, answer) = started.cloudformation(
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            ("Parameters.member.1.ParameterKey", POLICY_NAME_PARAMETER),
            ("Parameters.member.1.ParameterValue", &policy_name),
            ("Parameters.member.2.ParameterKey", MASTER_USER_PARAMETER),
            ("Parameters.member.2.ParameterValue", &master_user),
        ],
    );
    assert!(
        status == 200 && answer.contains("StackId"),
        "CreateStack must be accepted, got {status} with {answer}; the log:\n{}",
        started.log()
    );
    let (reported, settled) = started.settled_stack();
    assert_eq!(
        reported, CREATE_COMPLETE,
        "the declared estate must deploy, got {reported} with the resource failures {:?}; the \
         log:\n{}",
        started.failure_reasons(),
        started.log()
    );
    let outputs: BTreeMap<String, String> = outputs_of(&settled);
    let domain = outputs
        .get("DomainName")
        .cloned()
        .unwrap_or_else(|| panic!("the deployment published no domain name, got {outputs:?}"));
    let endpoint = outputs.get("DomainEndpoint").cloned().unwrap_or_default();
    assert!(
        !endpoint.is_empty(),
        "the deployment must publish the domain's endpoint, got {outputs:?}"
    );

    // The owning service holds a SETTLED domain at that address.
    let (status, described) =
        started.opensearch("GET", &format!("/2021-01-01/opensearch/domain/{domain}"));
    assert_eq!(
        status, 200,
        "the owning service must report the deployed domain, got {status} with {described}; \
         the log:\n{}",
        started.log()
    );
    let domain_status = described["DomainStatus"].clone();
    assert_eq!(
        (
            domain_status["Created"].as_bool(),
            domain_status["Processing"].as_bool(),
            domain_status["Endpoint"].as_str(),
        ),
        (Some(true), Some(false), Some(endpoint.as_str())),
        "a completed deployment must leave a created, settled domain published at the stack's \
         own endpoint output {endpoint:?}, got {domain_status}"
    );

    // The published address serves the cluster's own banner for the domain the stack created.
    let (status, banner) = started.at_endpoint(&endpoint, "GET", "/", None);
    assert_eq!(
        (status, banner["name"].as_str()),
        (200, Some(domain.as_str())),
        "a signed GET / on {endpoint:?} must be served for the deployed domain, got {status} \
         with {banner}; the log:\n{}",
        started.log()
    );

    // The plugin's audit configuration is readable and is written by the patch a client's
    // setup step sends, idempotently.
    for run in ["the first run", "a second, identical run"] {
        let (status, patched) =
            started.at_endpoint(&endpoint, "PATCH", AUDIT_PATH, Some(enabling_patch()));
        assert_eq!(
            status, 200,
            "case {run:?}: the enabling patch must be taken, got {status} with {patched}; the \
             log:\n{}",
            started.log()
        );
        let (status, read) = started.at_endpoint(&endpoint, "GET", AUDIT_PATH, None);
        assert_eq!(
            status, 200,
            "case {run:?}: the audit configuration must be readable, got {status} with {read}"
        );
        assert!(
            is_enabled(&read["config"]),
            "case {run:?}: auditing must read back enabled on both layers with no category \
             excluded, got {read}"
        );
    }

    // An index the cluster does not hold is created, and the identical repeat is refused as an
    // index that already exists.
    let index_path = format!("/{INDEX}");
    let (status, created) = started.at_endpoint(
        &endpoint,
        "PUT",
        &index_path,
        Some(json!({ "mappings": { "properties": { "title": { "type": "text" } } } })),
    );
    assert_eq!(
        status, 200,
        "PUT {index_path} must create the index, got {status} with {created}; the log:\n{}",
        started.log()
    );
    let (status, refused) = started.at_endpoint(&endpoint, "PUT", &index_path, Some(json!({})));
    assert_eq!(
        (status, refused["error"]["type"].as_str()),
        (400, Some("resource_already_exists_exception")),
        "the repeat of PUT {index_path} must be refused, got {status} with {refused}"
    );

    // The document the audited reads address.
    let document_path = format!("/{INDEX}/_doc/{DOCUMENT}");
    let (status, written) = started.at_endpoint(
        &endpoint,
        "PUT",
        &format!("{document_path}?refresh=true"),
        Some(json!({ "title": "an audited document" })),
    );
    assert_eq!(
        status, 201,
        "the seed document must be created, got {status} with {written}; the log:\n{}",
        started.log()
    );

    // From here on the records the group holds are the two reads' own.
    let audit_group = group_of(LOG_TYPES[0].1);
    let baseline = started.delivered(&audit_group).len();

    let (status, found) = started.at_endpoint(&endpoint, "GET", &document_path, None);
    assert_eq!(
        (status, found["found"].as_bool()),
        (200, Some(true)),
        "the seeded document must be read back, got {status} with {found}"
    );
    let absent_path = format!("/{INDEX}/_doc/{ABSENT}");
    let (status, missed) = started.at_endpoint(&endpoint, "GET", &absent_path, None);
    assert_eq!(
        (status, missed["found"].as_bool()),
        (404, Some(false)),
        "a read of an id the index does not hold must miss, got {status} with {missed}"
    );

    // Each read left ONE record per layer in the group the AUDIT_LOGS option names.
    let records = started.delivered_at_least(&audit_group, baseline + 4);
    let reads: Vec<Value> = records.into_iter().skip(baseline).collect();
    assert_eq!(
        reads.len(),
        4,
        "each read must leave one REST and one transport record, got {reads:#?}; the log:\n{}",
        started.log()
    );
    let rest_records = at_layer(&reads, "REST");
    let transport_records = at_layer(&reads, "TRANSPORT");
    assert_eq!(
        (rest_records.len(), transport_records.len()),
        (2, 2),
        "two records must be left at each layer, got {reads:#?}"
    );
    for (read, path) in [
        ("the served read", &document_path),
        ("the missed read", &absent_path),
    ] {
        let of_read: Vec<&Value> = rest_records
            .iter()
            .filter(|record| record["audit_rest_request_path"] == json!(path))
            .collect();
        assert_eq!(
            of_read.len(),
            1,
            "case {read:?}: exactly one REST record must name {path}, got {rest_records:#?}"
        );
        assert_eq!(
            (
                of_read[0]["audit_category"].as_str(),
                of_read[0]["audit_rest_request_method"].as_str(),
                of_read[0]["audit_request_effective_user"].as_str(),
            ),
            (
                Some(CATEGORY_AUTHENTICATED),
                Some("GET"),
                Some(master_user.as_str()),
            ),
            "case {read:?}: the REST record must state the admitted caller, got {:#?}",
            of_read[0]
        );
    }
    for record in &transport_records {
        assert_eq!(
            (
                record["audit_category"].as_str(),
                record["audit_transport_request_type"].as_str(),
                record["audit_trace_resolved_indices"].clone(),
            ),
            (
                Some(CATEGORY_GRANTED_PRIVILEGES),
                Some(GET_REQUEST),
                json!([INDEX]),
            ),
            "the transport record must state the granted read, got {record:#?}"
        );
    }

    // ...and an audit record reports the DECISION the cluster took, never the answer the
    // caller got.
    for record in &reads {
        let leaking = fields_reporting_the_answer(record);
        assert!(
            leaking.is_empty(),
            "an audit record must carry no field reporting the answer the request got, got \
             {leaking:?} in {record:#?}"
        );
    }

    // ...and no OTHER group the domain publishes to mentions either document.
    for (log_type, logical_id) in LOG_TYPES
        .iter()
        .filter(|(log_type, _)| *log_type != AUDIT_LOG_TYPE)
    {
        let group = group_of(logical_id);
        let held = started.delivered(&group);
        let mentioning: Vec<&Value> = held
            .iter()
            .filter(|record| {
                let text = record.to_string();
                text.contains(DOCUMENT) || text.contains(ABSENT)
            })
            .collect();
        assert!(
            mentioning.is_empty(),
            "the {log_type} group {group:?} must mention neither document, got {mentioning:#?}"
        );
    }
}
