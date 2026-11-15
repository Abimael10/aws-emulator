// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS this service PUBLISHES for an environment's Apache Airflow web server, end
//! to end over HTTP through the router this service mounts in production.
//!
//! The rule: an address an operation hands a caller to DIAL is the address this emulator is
//! reached at — the endpoint it serves this service's whole surface on — and never a host in
//! a zone Amazon Web Services publishes its own endpoints in, which resolves to nothing on
//! the machine this emulator runs on. It holds for every member that carries that address:
//! `Environment$WebserverUrl` on the read path, and the `WebServerHostname` both token
//! operations report beside a minted token, which is the HOST of that same URL — a client
//! that mints a CLI or web login token composes its next request on it.
//!
//! Each row varies what the rule generalises over: the endpoint the emulator is served at
//! (a loopback address with a port, a loopback NAME on the default port, a routable
//! interface, an IPv6 endpoint, and an emulator served on no port at all), the Region the
//! create is signed for, and the environment name. The `Hostname` shape's own constraints
//! are asserted on every published host, since a port and the brackets of an IPv6 literal
//! fit in no value of that shape.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter, UNSERVED_URL_BASE};

/// The canonical emulator account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The DNS zones AWS publishes its own service endpoints in. A published address under one
/// of them is the defect this suite pins: the emulator serves none of them.
/// (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES` states the same list
/// for the code under test; it is restated here so the assertion does not read its answer
/// from the implementation.)
const AWS_ENDPOINT_SUFFIXES: [&str; 4] =
    [".amazonaws.com", ".amazonaws.com.cn", ".api.aws", ".on.aws"];

/// `Hostname` is `@length(1, 255)`
/// (`smithy_query(query="shape",shape="Hostname",model_path="services/service-com-amazonaws-mwaa-amazonmwaa/smithy-model/com-amazonaws-mwaa-amazonmwaa.json")`).
const HOSTNAME_MAX_LENGTH: usize = 255;

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
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!("expected a JSON body, got {error}: {}", self.text())
        })
    }

    /// A top-level string member of the answer, or `None` when the answer does not carry it.
    fn member(&self, name: &str) -> Option<String> {
        self.json()
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// The emulator, assembled as the router assembles it: one shared engine behind one mounted
/// service, served at the endpoint the case names.
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    /// One emulator served at `endpoint` — the base URL a client addresses it at, which is
    /// what the production router records when its data port starts.
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

    /// One request, signed for `region` and addressed to `host`.
    async fn send(
        &self,
        region: &str,
        host: &str,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> Answer {
        let payload = body
            .map(|value| serde_json::to_vec(&value).expect("a serialisable body"))
            .unwrap_or_default();
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/airflow/aws4_request, \
             SignedHeaders=host, Signature=0000"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", host)
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        Answer {
            status: response.status().as_u16(),
            body: response.into_body().to_vec(),
        }
    }

    /// One request addressed to this service's own published control-plane host, which is
    /// where an SDK configured for AWS sends it.
    async fn call(&self, region: &str, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let host = format!("api.airflow.{region}.amazonaws.com");
        self.send(region, &host, method, uri, body).await
    }

    async fn create(&self, region: &str, name: &str) -> Answer {
        self.call(
            region,
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

    /// The `Environment` a `GetEnvironment` reports, read back through the implemented
    /// reader rather than from the create's own answer.
    async fn environment(&self, region: &str, name: &str) -> Value {
        let answer = self
            .call(region, "GET", &format!("/environments/{name}"), None)
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {region}/{name}: expected GetEnvironment 200, got {} {}",
            answer.status,
            answer.text()
        );
        answer
            .json()
            .get("Environment")
            .cloned()
            .unwrap_or_else(|| panic!("case {region}/{name}: GetEnvironment carried no Environment"))
    }
}

/// Whether `host` is a value the modelled `Hostname` admits: dot-separated labels of ASCII
/// letters, digits and inner hyphens, inside `@length(1, 255)`. Written out here from the
/// shape's own `@pattern`
/// (`^(([a-zA-Z0-9]|[a-zA-Z0-9][a-zA-Z0-9\-]*[a-zA-Z0-9])\.)*([A-Za-z0-9]|[A-Za-z0-9][A-Za-z0-9\-]*[A-Za-z0-9])$`)
/// so the assertion does not read its answer from the code under test.
fn satisfies_the_hostname_shape(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= HOSTNAME_MAX_LENGTH
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

/// One endpoint this emulator can be served at, and what an environment publishes there:
/// `(case, endpoint, published URL, published host)`. The host is `None` for an endpoint
/// whose host no `Hostname` value can spell.
type EndpointCase = (&'static str, &'static str, &'static str, Option<&'static str>);

/// The endpoints the rule is asserted over, spanning the forms the emulator's own base URL
/// takes: a loopback address with a port (what the production router records), a loopback
/// name on the default port, a routable interface an operator bound, an IPv6 endpoint, and
/// an emulator served on no port at all.
fn endpoint_cases() -> Vec<EndpointCase> {
    vec![
        (
            "a served loopback port",
            "https://127.0.0.1:9297",
            "https://127.0.0.1:9297",
            Some("127.0.0.1"),
        ),
        (
            "a loopback name on the default port",
            "https://localhost:443",
            "https://localhost:443",
            Some("localhost"),
        ),
        (
            "a routable interface",
            "https://10.0.0.7:18080",
            "https://10.0.0.7:18080",
            Some("10.0.0.7"),
        ),
        (
            "an emulator served on no port",
            UNSERVED_URL_BASE,
            "https://emulator.invalid",
            Some("emulator.invalid"),
        ),
        (
            "an IPv6 endpoint",
            "https://[::1]:9297",
            "https://[::1]:9297",
            None,
        ),
    ]
}

/// The Regions each case is replayed in: an environment lives in the Region its create was
/// signed for, and the address it publishes is this emulator's either way.
const REGIONS: [&str; 2] = ["us-east-1", "eu-west-2"];

/// The environment names each case is replayed with, spanning the forms
/// `EnvironmentName`'s `@pattern ^[a-zA-Z][0-9a-zA-Z-_]*$` admits.
const NAMES: [&str; 3] = ["MyMWAAEnvironment", "env-2_with-parts", "E"];

#[tokio::test]
async fn the_read_path_publishes_the_web_server_at_the_endpoint_this_emulator_is_served_at() {
    for (case, endpoint, published_url, _host) in endpoint_cases() {
        let harness = Harness::served_at(endpoint);
        for region in REGIONS {
            for name in NAMES {
                let row = format!("{case}/{region}/{name}");
                assert_eq!(
                    harness.create(region, name).await.status,
                    200,
                    "case {row:?}: the fixture create must succeed"
                );
                let environment = harness.environment(region, name).await;
                let url = environment
                    .get("WebserverUrl")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("case {row:?}: GetEnvironment reported no WebserverUrl"));
                assert_eq!(
                    url, published_url,
                    "case {row:?}: expected the emulator's own endpoint {published_url:?}, got \
                     {url:?}"
                );
                // `WebserverUrl` is `@pattern ^https://.+$` and `@length(1, 256)`.
                assert!(
                    url.starts_with("https://") && (1..=256).contains(&url.len()),
                    "case {row:?}: WebserverUrl must be an https URL inside its bound, got {url:?}"
                );
                for suffix in AWS_ENDPOINT_SUFFIXES {
                    assert!(
                        !url.ends_with(suffix),
                        "case {row:?}: a published address must not name an endpoint zone of \
                         AWS's ({suffix}), got {url:?}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn both_token_operations_report_the_host_of_the_published_web_server_url() {
    for (case, endpoint, published_url, published_host) in endpoint_cases() {
        let harness = Harness::served_at(endpoint);
        for region in REGIONS {
            for name in NAMES {
                let row = format!("{case}/{region}/{name}");
                assert_eq!(
                    harness.create(region, name).await.status,
                    200,
                    "case {row:?}: the fixture create must succeed"
                );
                // The URL the read path publishes, which the host member reports the host of.
                let url = harness
                    .environment(region, name)
                    .await
                    .get("WebserverUrl")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| panic!("case {row:?}: GetEnvironment reported no WebserverUrl"));
                assert_eq!(
                    url, published_url,
                    "case {row:?}: expected {published_url:?}, got {url:?}"
                );

                for (operation, uri) in [
                    ("CreateCliToken", format!("/clitoken/{name}")),
                    ("CreateWebLoginToken", format!("/webtoken/{name}")),
                ] {
                    let answer = harness.call(region, "POST", &uri, None).await;
                    assert_eq!(
                        answer.status,
                        200,
                        "case {row:?}/{operation}: expected 200 for a stored environment, got {} {}",
                        answer.status,
                        answer.text()
                    );
                    let reported = answer.member("WebServerHostname");
                    assert_eq!(
                        reported.as_deref(),
                        published_host,
                        "case {row:?}/{operation}: expected the host of {url:?} \
                         ({published_host:?}), got {reported:?}"
                    );
                    if let Some(reported) = &reported {
                        assert!(
                            satisfies_the_hostname_shape(reported),
                            "case {row:?}/{operation}: the reported host must satisfy the \
                             Hostname shape, got {reported:?}"
                        );
                        assert!(
                            url.contains(reported.as_str()),
                            "case {row:?}/{operation}: the reported host must be the host of \
                             {url:?}, got {reported:?}"
                        );
                        for suffix in AWS_ENDPOINT_SUFFIXES {
                            assert!(
                                !reported.ends_with(suffix),
                                "case {row:?}/{operation}: a published host must not name an \
                                 endpoint zone of AWS's ({suffix}), got {reported:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn the_published_host_is_a_host_this_emulator_answers_requests_addressed_to() {
    for (case, endpoint, _published_url, published_host) in endpoint_cases() {
        let Some(published_host) = published_host else {
            // An endpoint no `Hostname` can spell publishes no host; the URL beside it still
            // names the address, and the row above pins that.
            continue;
        };
        let harness = Harness::served_at(endpoint);
        let region = REGIONS[0];
        let name = NAMES[0];
        assert_eq!(
            harness.create(region, name).await.status,
            200,
            "case {case:?}: the fixture create must succeed"
        );
        let hostname = harness
            .call(region, "POST", &format!("/clitoken/{name}"), None)
            .await
            .member("WebServerHostname")
            .unwrap_or_else(|| panic!("case {case:?}: CreateCliToken reported no WebServerHostname"));
        assert_eq!(
            hostname, published_host,
            "case {case:?}: expected {published_host:?}, got {hostname:?}"
        );

        // The address handed to the caller is one this emulator answers on: a request
        // addressed to that very host reaches this service's environment, rather than
        // leaving for a host of AWS's that resolves elsewhere.
        let answer = harness
            .send(
                region,
                &hostname,
                "GET",
                &format!("/environments/{name}"),
                None,
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: a request addressed to the published host {hostname:?} must be \
             answered by this emulator, got {} {}",
            answer.status,
            answer.text()
        );
    }
}
