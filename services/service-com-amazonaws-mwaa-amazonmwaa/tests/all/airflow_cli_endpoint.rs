// SPDX-License-Identifier: CC0-1.0

//! The Apache Airflow CLI endpoint of an environment's web server — `POST
//! <WebServerHostname>/aws_mwaa/cli` — end to end over HTTP through the router this service
//! mounts in production.
//!
//! The rule: `CreateCliToken` mints a credential FOR that endpoint and reports the address
//! it answers at, so a client that presents the minted token as `Authorization: Bearer
//! <CliToken>` and posts an Apache Airflow CLI command to that address is answered HTTP 200
//! with a JSON document whose `stdout` (and `stderr`, when the command wrote to it) carries
//! the command's output base64-encoded — computed from the environment's own stored Apache
//! Airflow metadata. The token is the whole of what identifies the environment, so a token
//! authenticates to the environment that minted it and to no other, and a request presenting
//! none (or one nothing minted) reaches no command at all.
//!
//! Each row of the command table varies what the rule generalises over: the command GROUP
//! (`version`, `dags`, `variables`, `pools`), whether the command reports or CHANGES the
//! metadata, the output format (`-o table`'s default and `-o json`), and the usage errors a
//! command line can earn (an unknown group, an unknown command, a missing argument). The
//! change rows are read back through the environment's own Apache Airflow REST API
//! (`InvokeRestApi`), so one metadata database is shown behind both faces of it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use service_com_amazonaws_mwaa_amazonmwaa::state;

/// A signed-looking request header so the router's `can_handle` resolves this service by
/// its SigV4 credential scope, the way an SDK request does. Signature verification is off
/// in this harness, so the signature value is never checked.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTEST/20260101/us-east-1/airflow/aws4_request, \
     SignedHeaders=host, Signature=0000";

/// The canonical emulator account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The endpoint this harness's emulator is served at, which is where every environment's
/// Apache Airflow web server answers.
const EMULATOR_ENDPOINT: &str = "https://127.0.0.1:9297";

/// The address an environment's web server is published at under that endpoint: its
/// authority, which is what a client dials.
const PUBLISHED_ADDRESS: &str = "127.0.0.1:9297";

/// The path the Apache Airflow CLI endpoint is served at, as the Amazon MWAA documentation
/// spells it.
const CLI_PATH: &str = "/aws_mwaa/cli";

/// The media type a client sends an Apache Airflow CLI command as.
const COMMAND_MEDIA_TYPE: &str = "text/plain";

/// How often a poll reads a lifecycle it is waiting on, and how long it waits before it
/// calls the lifecycle broken. The deadline is generous: it bounds a condition that has to
/// HOLD, never how fast the host runs.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// One HTTP answer, reduced to what these tests assert on.
struct Answer {
    status: u16,
    challenge: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!("expected a JSON body, got {error}: {}", self.text())
        })
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
        String::from_utf8(decoded).expect("the CLI writes text")
    }
}

/// The emulator, assembled as the router assembles it: one shared engine behind one mounted
/// service, served at [`EMULATOR_ENDPOINT`].
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.set_url_base(
            EndpointUrl::parse(EMULATOR_ENDPOINT).expect("a usable emulator endpoint"),
        );
        router.register_service(
            service_com_amazonaws_mwaa_amazonmwaa::EmulatedService::new(Arc::clone(&engine)),
        );
        Self { router }
    }

    /// One AWS API request, signed the way an SDK signs one.
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let payload = body
            .map(|value| serde_json::to_vec(&value).expect("a serialisable body"))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "api.airflow.us-east-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        self.answered(request).await
    }

    /// One request to an environment's Apache Airflow CLI endpoint: addressed to the
    /// published web-server address, carrying the command as its body and the minted token
    /// as a bearer credential — no SigV4 signature, which is the whole point of the token.
    async fn cli(&self, host: &str, token: Option<&str>, command: &str) -> Answer {
        self.cli_request(host, token, command, "POST", CLI_PATH).await
    }

    async fn cli_request(
        &self,
        host: &str,
        token: Option<&str>,
        command: &str,
        method: &str,
        path: &str,
    ) -> Answer {
        let mut request = http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", host)
            .header("content-type", COMMAND_MEDIA_TYPE);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = request
            .body(bytes::Bytes::from(command.as_bytes().to_vec()))
            .expect("a well-formed request");
        self.answered(request).await
    }

    async fn answered(&self, request: http::Request<bytes::Bytes>) -> Answer {
        let response = self.router.dispatch(request).await;
        let challenge = response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        Answer {
            status: response.status().as_u16(),
            challenge,
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

    /// The status `GetEnvironment` reports for an environment.
    async fn status_of(&self, name: &str) -> String {
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
            .pointer("/Environment/Status")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("GetEnvironment({name}) reported no Status"))
            .to_string()
    }

    /// Wait for the CONDITION this suite needs of a fixture environment: that it has
    /// settled into `AVAILABLE`, which is when its web server is up.
    async fn wait_until_available(&self, name: &str) {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let status = self.status_of(name).await;
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

    /// `CreateCliToken`: the minted token and the address it is to be presented at.
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
        let host = answer
            .member("WebServerHostname")
            .unwrap_or_else(|| panic!("CreateCliToken({name}) reported no WebServerHostname"));
        (token, host)
    }

    /// One call to the environment's own Apache Airflow REST API, through the modelled
    /// operation — the OTHER face of the metadata database the CLI reads and writes.
    async fn rest_api(&self, name: &str, method: &str, path: &str) -> Answer {
        self.call(
            "POST",
            &format!("/restapi/{name}"),
            Some(json!({"Method": method, "Path": path})),
        )
        .await
    }
}

#[tokio::test]
async fn a_minted_token_runs_a_command_against_the_environments_own_metadata() {
    let harness = Harness::new();
    let name = "CliEnv";
    assert_eq!(
        harness.create(name).await.status,
        200,
        "the fixture create must succeed"
    );
    harness.wait_until_available(name).await;

    let (token, host) = harness.cli_token(name).await;
    assert_eq!(
        host, PUBLISHED_ADDRESS,
        "the token must be reported with the address the web server answers at, got {host:?}"
    );

    // The documented round trip: the command the Amazon MWAA documentation sends.
    let answered = harness.cli(&host, Some(&token), "dags list").await;
    assert_eq!(
        answered.status,
        200,
        "case \"dags list\": expected 200, got {} {}",
        answered.status,
        answered.text()
    );
    let stdout = answered.stream("stdout");
    assert!(
        stdout.contains("dag_id") || stdout.contains("No data found"),
        "case \"dags list\": expected the Apache Airflow dag table, got {stdout:?}"
    );
    assert_eq!(
        answered.stream("stderr"),
        "",
        "case \"dags list\": a served command writes nothing to standard error, got {:?}",
        answered.stream("stderr")
    );

    // (case, command, what stdout must contain, what stderr must contain)
    let rows: Vec<(&str, String, String, String)> = vec![
        (
            "the Apache Airflow version of the stored configuration",
            "version".to_string(),
            state::LATEST_AIRFLOW_VERSION.to_string(),
            String::new(),
        ),
        (
            "the CLI's own program name in front of the command",
            "airflow version".to_string(),
            state::LATEST_AIRFLOW_VERSION.to_string(),
            String::new(),
        ),
        (
            "the slot pool Apache Airflow seeds the database with",
            "pools list".to_string(),
            state::DEFAULT_POOL_NAME.to_string(),
            String::new(),
        ),
        (
            "no variables yet",
            "variables list".to_string(),
            "No data found".to_string(),
            String::new(),
        ),
        (
            "a variable set through the CLI",
            "variables set greeting \"hello world\"".to_string(),
            "Variable greeting created".to_string(),
            String::new(),
        ),
        (
            "that variable read back",
            "variables get greeting".to_string(),
            "hello world".to_string(),
            String::new(),
        ),
        (
            "that variable listed",
            "variables list".to_string(),
            "greeting".to_string(),
            String::new(),
        ),
        (
            "that variable listed as JSON",
            "variables list -o json".to_string(),
            "[{\"key\":\"greeting\"}]".to_string(),
            String::new(),
        ),
        (
            "a group this CLI does not serve",
            "celery worker".to_string(),
            String::new(),
            "invalid choice: 'celery'".to_string(),
        ),
        (
            "a command the group does not serve",
            "dags trigger".to_string(),
            String::new(),
            "invalid choice: 'trigger'".to_string(),
        ),
        (
            "a command missing its argument",
            "variables get".to_string(),
            String::new(),
            "the following arguments are required: key".to_string(),
        ),
        (
            "an empty command line",
            String::new(),
            String::new(),
            "the following arguments are required: GROUP_OR_COMMAND".to_string(),
        ),
    ];
    for (case, command, expected_out, expected_err) in rows {
        let answered = harness.cli(&host, Some(&token), &command).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: a command the endpoint ran is a 200, got {} {}",
            answered.status,
            answered.text()
        );
        let stdout = answered.stream("stdout");
        let stderr = answered.stream("stderr");
        assert!(
            stdout.contains(&expected_out),
            "case {case:?}: expected stdout to contain {expected_out:?}, got {stdout:?}"
        );
        if expected_err.is_empty() {
            assert_eq!(
                stderr, "",
                "case {case:?}: expected no standard error, got {stderr:?}"
            );
        } else {
            assert!(
                stderr.contains(&expected_err),
                "case {case:?}: expected stderr to contain {expected_err:?}, got {stderr:?}"
            );
        }
    }

    // The change the CLI made is in the ONE metadata database the environment's own Apache
    // Airflow REST API reads: both faces read and write the same store.
    let read_back = harness.rest_api(name, "GET", "/variables/greeting").await;
    assert_eq!(
        read_back.status,
        200,
        "the REST API must report the variable the CLI set, got {} {}",
        read_back.status,
        read_back.text()
    );
    assert_eq!(
        read_back.json().pointer("/RestApiResponse/value"),
        Some(&json!("hello world")),
        "the REST API must report the value the CLI stored, got {}",
        read_back.text()
    );

    // And the other way round: a variable written through the REST API is listed by the CLI.
    let written = harness
        .call(
            "POST",
            &format!("/restapi/{name}"),
            Some(json!({
                "Method": "POST",
                "Path": "/variables",
                "Body": {"key": "written_through_the_api", "value": "7"},
            })),
        )
        .await;
    assert_eq!(
        written.status,
        200,
        "the REST API write must succeed, got {} {}",
        written.status,
        written.text()
    );
    let listed = harness
        .cli(&host, Some(&token), "variables list")
        .await
        .stream("stdout");
    assert!(
        listed.contains("written_through_the_api"),
        "the CLI must list the variable the REST API wrote, got {listed:?}"
    );
}

#[tokio::test]
async fn a_token_authenticates_to_the_environment_that_minted_it_and_to_no_other() {
    let harness = Harness::new();
    for name in ["CliTokenEnvOne", "CliTokenEnvTwo"] {
        assert_eq!(
            harness.create(name).await.status,
            200,
            "case {name}: the fixture create must succeed"
        );
    }
    harness.wait_until_available("CliTokenEnvOne").await;
    harness.wait_until_available("CliTokenEnvTwo").await;

    let (token_one, host) = harness.cli_token("CliTokenEnvOne").await;
    let (token_two, _) = harness.cli_token("CliTokenEnvTwo").await;
    assert_ne!(
        token_one, token_two,
        "each CreateCliToken must mint a token of its own"
    );

    // Each token runs its command against ITS OWN environment's metadata: the token is the
    // whole of what identifies the environment, since one address serves both web servers.
    for (case, token, key) in [
        ("the first environment's token", &token_one, "only_in_one"),
        ("the second environment's token", &token_two, "only_in_two"),
    ] {
        let answered = harness
            .cli(&host, Some(token), &format!("variables set {key} x"))
            .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected 200, got {} {}",
            answered.status,
            answered.text()
        );
    }
    for (case, token, held, absent) in [
        (
            "the first environment",
            &token_one,
            "only_in_one",
            "only_in_two",
        ),
        (
            "the second environment",
            &token_two,
            "only_in_two",
            "only_in_one",
        ),
    ] {
        let listed = harness
            .cli(&host, Some(token), "variables list")
            .await
            .stream("stdout");
        assert!(
            listed.contains(held) && !listed.contains(absent),
            "case {case:?}: expected the variables of that environment alone ({held}), got \
             {listed:?}"
        );
    }

    // A request presenting no credential, or one nothing minted, reaches no command.
    let unauthenticated = harness.cli(&host, None, "dags list").await;
    assert_eq!(
        unauthenticated.status,
        401,
        "a request presenting no token must be refused, got {} {}",
        unauthenticated.status,
        unauthenticated.text()
    );
    assert_eq!(
        unauthenticated.challenge.as_deref(),
        Some("Bearer"),
        "the refusal must state the scheme a token is presented under, got {:?}",
        unauthenticated.challenge
    );
    for (case, presented) in [
        ("a token nothing minted", "not-a-token-this-emulator-minted"),
        ("an empty bearer value", ""),
    ] {
        let refused = harness.cli(&host, Some(presented), "dags list").await;
        let expected = if presented.is_empty() { 401 } else { 403 };
        assert_eq!(
            refused.status, expected,
            "case {case:?}: expected {expected}, got {} {}",
            refused.status,
            refused.text()
        );
    }

    // The endpoint serves the command in the request BODY, so another method on that path
    // is refused rather than answered.
    let wrong_method = harness
        .cli_request(&host, Some(&token_one), "", "GET", CLI_PATH)
        .await;
    assert_eq!(
        wrong_method.status,
        405,
        "a GET of the CLI endpoint must be refused, got {} {}",
        wrong_method.status,
        wrong_method.text()
    );
}

#[tokio::test]
async fn a_command_is_refused_while_the_environments_web_server_is_not_up() {
    let harness = Harness::new();
    let name = "CliEnvPending";
    // An environment whose VPC endpoints the CALLER owns stays in a non-terminal create
    // status until those endpoints exist, and no operation of this service reports that they
    // do — so its web server is not up, and stays not up, whatever the clock does. That is
    // the condition this row needs, rather than a status that only holds for a span.
    let created = harness
        .call(
            "PUT",
            &format!("/environments/{name}"),
            Some(json!({
                "ExecutionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/mwaa-execution"),
                "SourceBucketArn": "arn:aws:s3:::my-airflow-bucket",
                "DagS3Path": "dags",
                "EndpointManagement": "CUSTOMER",
                "NetworkConfiguration": {
                    "SubnetIds": ["subnet-00000000000000001", "subnet-00000000000000002"],
                    "SecurityGroupIds": ["sg-00000000000000001"],
                },
            })),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "the fixture create must succeed, got {} {}",
        created.status,
        created.text()
    );

    let status = harness.status_of(name).await;
    assert_eq!(
        status,
        state::STATUS_PENDING,
        "the fixture environment must be waiting for the caller's VPC endpoints, got {status}"
    );
    // The token is minted for a stored environment whatever its status; the command against a
    // web server that is not serving is refused rather than answered out of a database no
    // server is behind.
    let (token, host) = harness.cli_token(name).await;
    let refused = harness.cli(&host, Some(&token), "dags list").await;
    assert_eq!(
        refused.status,
        503,
        "a command addressed to a web server that is not up must be refused, got {} {}",
        refused.status,
        refused.text()
    );
    assert!(
        refused
            .member("message")
            .unwrap_or_default()
            .contains(state::STATUS_PENDING),
        "the refusal must state the status the environment is in, got {}",
        refused.text()
    );

    // And an environment whose web server IS up serves the same command with the same kind
    // of token, so the refusal above is the status and not the endpoint.
    let serving = "CliEnvServing";
    assert_eq!(
        harness.create(serving).await.status,
        200,
        "the second fixture create must succeed"
    );
    harness.wait_until_available(serving).await;
    let (serving_token, host) = harness.cli_token(serving).await;
    let answered = harness.cli(&host, Some(&serving_token), "dags list").await;
    assert_eq!(
        answered.status,
        200,
        "the same command must be served by an environment that is available, got {} {}",
        answered.status,
        answered.text()
    );
}
