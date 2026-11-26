// SPDX-License-Identifier: CC0-1.0

//! Probe: the OpenSearch Service node-maintenance flow a polling client drives, against the
//! emulator a customer STARTS — the built binary, on its own port, over HTTPS, with every
//! call signed as an SDK signs it.
//!
//! The sequence is the one an AWS-targeting client runs: create a three-data-node domain,
//! wait for it to report itself settled, read its node census, start a maintenance action on
//! a data node it published, poll the action's status until it is terminal, and read the
//! listing back. The status read is the one call of the flow that carries a QUERY STRING, so
//! this is also where the started emulator's front door is exercised with one.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The Region every call below is signed for, and OpenSearch Service's signing name.
const REGION: &str = "us-west-2";
const SIGNING_NAME: &str = "es";
const PREFIX: &str = "/2021-01-01";

/// How long the probe waits for the emulator to publish its files, and for a condition of the
/// flow to hold; and how often it looks meanwhile. Generous: what is asserted is that the
/// condition holds at all, never how fast the host runs.
const STARTUP_DEADLINE: Duration = Duration::from_secs(120);
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// Seconds of idleness after which an emulator this probe started stops on its own, so a
/// killed test leaves no process behind.
const ORPHAN_IDLE_TIMEOUT_SECS: &str = "300";

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

    fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("emulator.log")).unwrap_or_default()
    }

    /// One signed call on the emulator's front door, answered with its status and body.
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let host = format!("{SIGNING_NAME}.{REGION}.amazonaws.com");
        let authority = format!("127.0.0.1:{}", self.port);
        let payload = body
            .map(|value| value.to_string().into_bytes())
            .unwrap_or_default();
        let mut request = http::Request::builder()
            .method(method)
            .uri(format!("https://{authority}{path}"))
            .header("host", host.as_str());
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let digest = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&payload));
        request = request.header("x-amz-content-sha256", digest);
        let request = request.body(payload).expect("a well-formed request");
        let signed = self.signed(request);
        let response = self
            .client
            .execute(signed.try_into().expect("a request reqwest can send"))
            .expect("the emulator answers");
        let status = response.status().as_u16();
        let text = response.text().unwrap_or_default();
        let parsed = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, parsed)
    }

    fn signed(&self, mut request: http::Request<Vec<u8>>) -> http::Request<Vec<u8>> {
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
            .name(SIGNING_NAME)
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

fn emulator_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aws-emulator"))
}

fn new_home() -> PathBuf {
    static STARTS: AtomicU32 = AtomicU32::new(0);
    let name = format!("emulator-maintenance-probe-{}", std::process::id());
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

fn profile_value(text: &str, name: &str) -> Option<String> {
    let mut pairs = text.lines().filter_map(|line| line.split_once('='));
    let (_, value) = pairs.find(|(key, _)| key.trim() == name)?;
    Some(value.trim().to_string())
}

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

/// RULE: on the emulator a customer starts, a node maintenance action a client starts on a
/// published data node becomes terminal on the status reads that client makes, and the
/// listing reports the same action under the same terminal status.
#[test]
fn an_accepted_maintenance_action_becomes_terminal_on_a_started_emulator() {
    let started = Started::start();
    let name = "probe-maintained-domain";

    let (status, body) = started.call(
        "POST",
        &format!("{PREFIX}/opensearch/domain"),
        Some(&json!({
            "DomainName": name,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 3 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "AccessPolicies": "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\"Principal\":{\"AWS\":\"arn:aws:iam::123456789012:root\"},\"Action\":\"es:*\",\"Resource\":\"arn:aws:es:us-west-2:123456789012:domain/probe-maintained-domain/*\"}]}",
        })),
    );
    assert_eq!(
        status, 200,
        "CreateDomain: expected 200, got {status} with {body}; the log:\n{}",
        started.log()
    );

    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = started.call("GET", &format!("{PREFIX}/opensearch/domain/{name}"), None);
        assert_eq!(
            status, 200,
            "DescribeDomain: expected 200, got {status} with {body}"
        );
        if body["DomainStatus"]["Processing"].as_bool() == Some(false)
            && body["DomainStatus"]["Created"].as_bool() == Some(true)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "DescribeDomain: the domain never reported itself settled, last read {body}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    }

    let deadline = Instant::now() + SETTLE_DEADLINE;
    let node_id = loop {
        let (status, body) =
            started.call("GET", &format!("{PREFIX}/opensearch/domain/{name}/nodes"), None);
        let found = body["DomainNodesStatusList"]
            .as_array()
            .and_then(|nodes| nodes.first())
            .and_then(|node| node["NodeId"].as_str())
            .map(str::to_string);
        if let Some(found) = found.filter(|_| status == 200) {
            break found;
        }
        assert!(
            Instant::now() < deadline,
            "DescribeDomainNodes: no node id was ever published, last answer {status} with {body}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    };

    let (status, body) = started.call(
        "POST",
        &format!("{PREFIX}/opensearch/domain/{name}/domainMaintenance"),
        Some(&json!({ "Action": "RESTART_SEARCH_PROCESS", "NodeId": node_id })),
    );
    assert_eq!(
        status, 200,
        "StartDomainMaintenance({node_id}): expected 200, got {status} with {body}"
    );
    let maintenance_id = body["MaintenanceId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartDomainMaintenance: expected a MaintenanceId, got {body}"))
        .to_string();

    // The read that follows the call that started the action reports it under a status the
    // action is still under way in: the clock owes it its run.
    let (status, body) = started.call(
        "GET",
        &format!(
            "{PREFIX}/opensearch/domain/{name}/domainMaintenance?maintenanceId={maintenance_id}"
        ),
        None,
    );
    assert_eq!(
        status, 200,
        "GetDomainMaintenanceStatus: expected 200 from the read after the start, got {status} \
         with {body}"
    );
    assert!(
        matches!(body["Status"].as_str(), Some("PENDING" | "IN_PROGRESS")),
        "GetDomainMaintenanceStatus: expected the action to be read running, got {body}"
    );

    let deadline = Instant::now() + SETTLE_DEADLINE;
    let terminal = loop {
        let (status, body) = started.call(
            "GET",
            &format!(
                "{PREFIX}/opensearch/domain/{name}/domainMaintenance?maintenanceId={maintenance_id}"
            ),
            None,
        );
        assert_eq!(
            status, 200,
            "GetDomainMaintenanceStatus: expected 200 from every poll, got {status} with {body}"
        );
        let reported = body["Status"].as_str().unwrap_or_default().to_string();
        if !matches!(reported.as_str(), "PENDING" | "IN_PROGRESS") {
            break reported;
        }
        assert!(
            Instant::now() < deadline,
            "GetDomainMaintenanceStatus: the action never reached a terminal status, last {body}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    };
    assert_eq!(
        terminal, "SUCCESS",
        "GetDomainMaintenanceStatus: expected the terminal status a finished action reports"
    );

    let (status, body) = started.call(
        "GET",
        &format!("{PREFIX}/opensearch/domain/{name}/domainMaintenances"),
        None,
    );
    assert_eq!(
        status, 200,
        "ListDomainMaintenances: expected 200, got {status} with {body}"
    );
    let listed = body["DomainMaintenances"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        listed.iter().any(|row| {
            row["MaintenanceId"].as_str() == Some(maintenance_id.as_str())
                && row["Status"].as_str() == Some("SUCCESS")
        }),
        "ListDomainMaintenances: expected the started action listed as finished, got {listed:?}"
    );
}
