// SPDX-License-Identifier: CC0-1.0

//! The address `CreateCliToken` hands a caller, taken as the ONLY thing that client knows:
//! it composes `https://<WebServerHostname>/aws_mwaa/cli` on it and dials it, which is the
//! documented use of the token/host pair.
//!
//! The rule: an operation that publishes an address for a caller to DIAL publishes the
//! address this emulator is actually reached at — the whole authority of its endpoint, the
//! port included wherever one has to be dialled — so the URL a client composes on the
//! published value addresses the very socket this emulator answers on, and the request it
//! sends there is served. A host published without the port it is served on names the
//! scheme's default port, which nothing listens on, so the composed URL reaches nothing;
//! that is what these rows pin.
//!
//! Each row varies what the rule generalises over: the endpoint form this emulator is served
//! at (the port the production router binds, another port an operator chose, a loopback NAME
//! on the scheme's own default port, a routable interface, an IPv6 literal, and an emulator
//! served on no port at all), and the Apache Airflow CLI command the composed URL is driven
//! with. The expected address of each row is built from that row's own host and port, so
//! nothing here reads its answer out of the implementation.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter, UNSERVED_URL_BASE};

use service_com_amazonaws_mwaa_amazonmwaa::state;

/// The canonical emulator account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The path an environment's Apache Airflow web server serves the CLI endpoint at, as the
/// Amazon MWAA documentation spells it.
const CLI_PATH: &str = "/aws_mwaa/cli";

/// The media type a client sends an Apache Airflow CLI command as.
const COMMAND_MEDIA_TYPE: &str = "text/plain";

/// How often a poll reads the lifecycle it waits on, and how long it waits before calling
/// that lifecycle broken. The deadline is generous: it bounds a condition that has to HOLD,
/// never how fast the host runs.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// One endpoint this emulator can be served at: `(case, host, port)`. The endpoint URL and
/// the address a client must be handed are both built from the host and port here.
type ServedCase = (&'static str, &'static str, Option<u16>);

/// The host of an emulator served on no port at all, read off the runtime's own base URL for
/// that case rather than restated, so this row cannot drift from it.
fn unserved_host() -> &'static str {
    UNSERVED_URL_BASE
        .strip_prefix("https://")
        .expect("the base URL of an unserved emulator is an https URL")
}

/// The endpoint forms the rule is asserted over, including an emulator served on no port at
/// all, where there is no port to publish.
fn served_cases() -> Vec<ServedCase> {
    vec![
        (
            "the port the production router binds",
            "127.0.0.1",
            Some(9297),
        ),
        ("another port an operator chose", "127.0.0.1", Some(41357)),
        (
            "a loopback name on the scheme's default port",
            "localhost",
            Some(443),
        ),
        ("a routable interface", "10.0.0.7", Some(18080)),
        ("an IPv6 literal", "[::1]", Some(9297)),
        ("an emulator served on no port", unserved_host(), None),
    ]
}

/// The base URL this emulator is served at for one case.
fn endpoint_of(host: &str, port: Option<u16>) -> String {
    match port {
        Some(port) => format!("https://{host}:{port}"),
        None => format!("https://{host}"),
    }
}

/// The address a client has to be handed for one case: the authority of that endpoint, which
/// is the host together with the port a client dials.
fn expected_address(host: &str, port: Option<u16>) -> String {
    match port {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

/// One HTTP answer, reduced to what this suite asserts on.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("expected a JSON body, got {error}: {}", self.text()))
    }

    /// A top-level string member of the answer.
    fn member(&self, name: &str) -> Option<String> {
        self.json()
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// One output stream of a CLI answer, base64-decoded — the empty string when the answer
    /// carries no such member, which is what a stream the command wrote nothing to is.
    fn stream(&self, member: &str) -> String {
        let Some(encoded) = self.member(member) else {
            return String::new();
        };
        let decoded = ::base64::Engine::decode(
            &::base64::engine::general_purpose::STANDARD,
            encoded.as_bytes(),
        )
        .unwrap_or_else(|error| panic!("{member} must be base64, got {encoded:?}: {error}"));
        String::from_utf8(decoded).expect("the Apache Airflow CLI writes text")
    }
}

/// The emulator, assembled as the router assembles it: one shared engine behind one mounted
/// service, served at the endpoint the case names.
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    fn served_at(endpoint: &str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.set_url_base(
            EndpointUrl::parse(endpoint)
                .unwrap_or_else(|| panic!("{endpoint:?} must be a usable emulator endpoint")),
        );
        router.register_service(
            service_com_amazonaws_mwaa_amazonmwaa::EmulatedService::new(Arc::clone(&engine)),
        );
        Self { router }
    }

    /// One AWS API request, addressed and signed the way an SDK configured for AWS sends one.
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let payload = body
            .map(|value| serde_json::to_vec(&value).expect("a serialisable body"))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "api.airflow.us-east-1.amazonaws.com")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDTEST/20260101/us-east-1/airflow/aws4_request, \
                 SignedHeaders=host, Signature=0000",
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        self.answered(request).await
    }

    /// One request a client composes on the PUBLISHED address alone: the absolute target
    /// `https://<address><CLI_PATH>` it would dial, addressed to that same authority, with
    /// the minted token as its whole credential.
    async fn dialed(&self, address: &str, token: &str, command: &str) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{address}{CLI_PATH}"))
            .header("host", address)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", COMMAND_MEDIA_TYPE)
            .body(bytes::Bytes::from(command.as_bytes().to_vec()))
            .expect("a well-formed request");
        self.answered(request).await
    }

    async fn answered(&self, request: http::Request<bytes::Bytes>) -> Answer {
        let response = self.router.dispatch(request).await;
        Answer {
            status: response.status().as_u16(),
            body: response.into_body().to_vec(),
        }
    }

    async fn create(&self, name: &str) -> Answer {
        self.call(
            "PUT",
            &format!("/environments/{name}"),
            Some(json!({
                "ExecutionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/mwaa-execution"),
                "SourceBucketArn": "arn:aws:s3:::my-airflow-bucket",
                "DagS3Path": "dags",
                "NetworkConfiguration": {
                    "SubnetIds": ["subnet-00000000000000001", "subnet-00000000000000002"],
                    "SecurityGroupIds": ["sg-00000000000000001"],
                },
            })),
        )
        .await
    }

    /// The `Environment` document `GetEnvironment` reports.
    async fn environment(&self, name: &str) -> Value {
        let answer = self.call("GET", &format!("/environments/{name}"), None).await;
        assert_eq!(
            answer.status,
            200,
            "GetEnvironment({name}) must succeed, got {} {}",
            answer.status,
            answer.text()
        );
        answer
            .json()
            .get("Environment")
            .cloned()
            .unwrap_or_else(|| panic!("GetEnvironment({name}) carried no Environment"))
    }

    /// Wait for the CONDITION the round trip needs of a fixture environment: that it has
    /// settled into `AVAILABLE`, which is when its web server is up.
    async fn wait_until_available(&self, name: &str) {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let status = self
                .environment(name)
                .await
                .get("Status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if status == state::STATUS_AVAILABLE {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{name} never reached {}, last status {status}",
                state::STATUS_AVAILABLE
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// `CreateCliToken`: the minted token and the address reported beside it.
    async fn cli_token(&self, name: &str) -> (String, String) {
        let answer = self.call("POST", &format!("/clitoken/{name}"), None).await;
        assert_eq!(
            answer.status,
            200,
            "CreateCliToken({name}) must succeed, got {} {}",
            answer.status,
            answer.text()
        );
        let token = answer
            .member("CliToken")
            .unwrap_or_else(|| panic!("CreateCliToken({name}) minted no CliToken"));
        let address = answer
            .member("WebServerHostname")
            .unwrap_or_else(|| panic!("CreateCliToken({name}) reported no WebServerHostname"));
        (token, address)
    }
}

/// The URL a client composes on a published address, read with the `http` crate's own parse
/// rather than by hand.
fn composed(address: &str) -> ::http::Uri {
    format!("https://{address}{CLI_PATH}")
        .parse::<::http::Uri>()
        .unwrap_or_else(|error| {
            panic!("a published address must compose a URL, got {address:?}: {error}")
        })
}

#[tokio::test]
async fn the_url_a_client_composes_on_the_published_address_names_the_port_this_emulator_is_served_on()
{
    for (case, host, port) in served_cases() {
        let harness = Harness::served_at(&endpoint_of(host, port));
        let name = "CliAddressEnv";
        assert_eq!(
            harness.create(name).await.status,
            200,
            "case {case:?}: the fixture create must succeed"
        );
        let (_, address) = harness.cli_token(name).await;
        let expected = expected_address(host, port);
        assert_eq!(
            address, expected,
            "case {case:?}: expected the address this emulator is reached at ({expected:?}), \
             got {address:?}"
        );

        // The composition the documented use of the pair performs, taken as the primary
        // fact: the URL a client builds on the published value dials the port this emulator
        // is served on — never the scheme's default, which is where a published host
        // stripped of its port sends a client — and addresses the CLI endpoint's path.
        let url = composed(&address);
        assert_eq!(
            url.port_u16(),
            port,
            "case {case:?}: the composed URL must dial the port this emulator is served on \
             ({port:?}), got {:?} in {url}",
            url.port_u16()
        );
        assert_eq!(
            url.path(),
            CLI_PATH,
            "case {case:?}: the composed URL must address the CLI endpoint, got {url}"
        );

        // Both members name one web server, so the read path publishes the same address.
        let reported = harness
            .environment(name)
            .await
            .get("WebserverUrl")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| panic!("case {case:?}: GetEnvironment reported no WebserverUrl"));
        assert_eq!(
            reported, address,
            "case {case:?}: the read path must publish the same address as the token \
             operation ({address:?}), got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_client_holding_only_the_published_address_runs_the_documented_cli_round_trip() {
    // (case, command, the lines the command's standard output may carry — the Apache Airflow
    // CLI's own answers: the dag table's header column, or the empty-table line for an
    // environment whose DAG folder holds none; the stored configuration's Apache Airflow
    // version; and the slot pool Apache Airflow seeds its metadata database with).
    let commands: Vec<(&str, &str, Vec<String>)> = vec![
        (
            "the command the documentation sends",
            "dags list",
            vec!["dag_id".to_string(), "No data found".to_string()],
        ),
        (
            "the Apache Airflow version of the stored configuration",
            "version",
            vec![state::LATEST_AIRFLOW_VERSION.to_string()],
        ),
        (
            "the slot pool Apache Airflow seeds its database with",
            "pools list",
            vec![state::DEFAULT_POOL_NAME.to_string()],
        ),
    ];

    for (case, host, port) in served_cases() {
        let harness = Harness::served_at(&endpoint_of(host, port));
        let name = "CliRoundTripEnv";
        assert_eq!(
            harness.create(name).await.status,
            200,
            "case {case:?}: the fixture create must succeed"
        );
        harness.wait_until_available(name).await;

        let (token, address) = harness.cli_token(name).await;
        assert_eq!(
            address,
            expected_address(host, port),
            "case {case:?}: expected the address this emulator is reached at, got {address:?}"
        );

        for (command_case, command, expected_lines) in &commands {
            let row = format!("{case}/{command_case}");
            // Dialled at the authority the client composed, which is the one thing it was
            // handed: a request addressed there has to be served by this emulator.
            let answered = harness.dialed(&address, &token, command).await;
            assert_eq!(
                answered.status,
                200,
                "case {row:?}: a command posted to the published address must be answered, \
                 got {} {}",
                answered.status,
                answered.text()
            );
            let stdout = answered.stream("stdout");
            assert!(
                expected_lines.iter().any(|line| stdout.contains(line)),
                "case {row:?}: expected standard output to carry one of {expected_lines:?}, \
                 got {stdout:?}"
            );
            let stderr = answered.stream("stderr");
            assert_eq!(
                stderr, "",
                "case {row:?}: a served command writes nothing to standard error, got \
                 {stderr:?}"
            );
        }
    }
}
