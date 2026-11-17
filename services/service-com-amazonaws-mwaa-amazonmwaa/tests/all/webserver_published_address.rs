// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS this service PUBLISHES for an environment's Apache Airflow web server, end
//! to end over HTTP through the router this service mounts in production.
//!
//! The rule: the address an operation hands a caller to DIAL is the address this emulator is
//! reached at — the authority of the endpoint it serves this service's whole surface on,
//! host and port together — carried with no scheme, and never a host in a zone Amazon Web
//! Services publishes its own endpoints in, which resolves to nothing on the machine this
//! emulator runs on. It holds for every member that carries that address, and both carry the
//! SAME value, because both name one web server: `Environment$WebserverUrl` on the read path
//! ("the Apache Airflow web server HOST NAME for the Amazon MWAA environment") and the
//! `WebServerHostname` both token operations report beside a minted token, on which a client
//! composes its next request (`https://<address>/aws_mwaa/cli`).
//!
//! Each row varies what the rule generalises over: the endpoint the emulator is served at
//! (a loopback address with a port, a loopback NAME on the default port, a routable
//! interface, an IPv6 endpoint, and an emulator served on no port at all), the Region the
//! create is signed for, and the environment name.

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
/// (`smithy_query(query="shape",shape="Hostname",model_path="services/service-com-amazonaws-mwaa-amazonmwaa/smithy-model/com-amazonaws-mwaa-amazonmwaa.json"`).
/// `WebserverUrl` is `@length(1, 256)`, so the published address stays inside the shorter of
/// the two.
const PUBLISHED_ADDRESS_MAX_LENGTH: usize = 255;

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

/// Whether `https://<address>/<path>` — the composition every client of a published address
/// performs — is a URL whose authority is that address, which is what makes the published
/// value usable at all. Read with the `http` crate's own URI parse rather than by hand.
fn is_composable_address(address: &str) -> bool {
    let Ok(uri) = format!("https://{address}/aws_mwaa/cli").parse::<::http::Uri>() else {
        return false;
    };
    uri.authority().map(::http::uri::Authority::as_str) == Some(address)
        && uri.path() == "/aws_mwaa/cli"
}

/// One endpoint this emulator can be served at, and the address an environment publishes
/// there: `(case, endpoint, published address)`.
type EndpointCase = (&'static str, &'static str, &'static str);

/// The endpoints the rule is asserted over, spanning the forms the emulator's own base URL
/// takes: a loopback address with a port (what the production router records), a loopback
/// name on the default port, a routable interface an operator bound, an IPv6 endpoint, and
/// an emulator served on no port at all.
fn endpoint_cases() -> Vec<EndpointCase> {
    vec![
        (
            "a served loopback port",
            "https://127.0.0.1:9297",
            "127.0.0.1:9297",
        ),
        (
            "a loopback name on the default port",
            "https://localhost:443",
            "localhost:443",
        ),
        (
            "a routable interface",
            "https://10.0.0.7:18080",
            "10.0.0.7:18080",
        ),
        (
            "an emulator served on no port",
            UNSERVED_URL_BASE,
            "emulator.invalid",
        ),
        ("an IPv6 endpoint", "https://[::1]:9297", "[::1]:9297"),
    ]
}

/// The Regions each case is replayed in: an environment lives in the Region its create was
/// signed for, and the address it publishes is this emulator's either way.
const REGIONS: [&str; 2] = ["us-east-1", "eu-west-2"];

/// The environment names each case is replayed with, spanning the forms
/// `EnvironmentName`'s `@pattern ^[a-zA-Z][0-9a-zA-Z-_]*$` admits.
const NAMES: [&str; 3] = ["MyMWAAEnvironment", "env-2_with-parts", "E"];

/// RULE, stated as the deliberate deviation it is: the published address carries the PORT
/// this emulator is served on, and no value of the modelled `Hostname` shape can spell one —
/// its `@pattern`
/// (`^(([a-zA-Z0-9]|[a-zA-Z0-9][a-zA-Z0-9\-]*[a-zA-Z0-9])\.)*([A-Za-z0-9]|…)$`) admits
/// neither a colon nor the brackets of an IPv6 literal. The port is carried anyway, for both
/// members, because what they name is an address a client DIALS: a host without the port it
/// is served on names a port nothing listens on, which is the defect this fix is about (the
/// Apache Airflow CLI endpoint is reached at exactly this address). The bound both members do
/// keep is the length one, which every published address stays inside.
///
/// The addresses are the ones the tests below read back out of the service for each endpoint
/// form, so this states a property of what the service publishes and not of a literal.
#[test]
fn the_published_address_carries_the_port_the_hostname_shape_cannot_spell() {
    for (case, endpoint, published) in endpoint_cases() {
        let served_port = endpoint.rsplit_once(':').and_then(|(_, port)| {
            port.parse::<u16>().ok().map(|port| port.to_string())
        });
        match served_port {
            Some(port) => {
                assert!(
                    published.ends_with(&format!(":{port}")),
                    "case {case:?}: the published address must carry the port a client has \
                     to dial ({port}), got {published:?}"
                );
                assert!(
                    published.contains(':'),
                    "case {case:?}: that port makes the value one no Hostname @pattern \
                     admits, which is the deviation this rule states, got {published:?}"
                );
            }
            None => assert!(
                !published.contains(':'),
                "case {case:?}: an emulator served on no port publishes no port, got \
                 {published:?}"
            ),
        }
        assert!(
            (1..=PUBLISHED_ADDRESS_MAX_LENGTH).contains(&published.len()),
            "case {case:?}: the published address stays inside the members' length bound, \
             got {published:?}"
        );
    }
}

#[tokio::test]
async fn the_read_path_publishes_the_web_server_at_the_endpoint_this_emulator_is_served_at() {
    for (case, endpoint, published) in endpoint_cases() {
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
                    url, published,
                    "case {row:?}: expected the authority of this emulator's own endpoint \
                     {published:?}, got {url:?}"
                );
                // The member is the web server's HOST NAME, so it carries no scheme — and a
                // client composes its own URL on it.
                assert!(
                    !url.contains("://"),
                    "case {row:?}: WebserverUrl is a host name, not a URL, got {url:?}"
                );
                assert!(
                    is_composable_address(url) && (1..=PUBLISHED_ADDRESS_MAX_LENGTH).contains(&url.len()),
                    "case {row:?}: WebserverUrl must be an address a client can compose a URL \
                     on, inside its bound, got {url:?}"
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
async fn both_token_operations_report_the_published_web_server_address() {
    for (case, endpoint, published) in endpoint_cases() {
        let harness = Harness::served_at(endpoint);
        for region in REGIONS {
            for name in NAMES {
                let row = format!("{case}/{region}/{name}");
                assert_eq!(
                    harness.create(region, name).await.status,
                    200,
                    "case {row:?}: the fixture create must succeed"
                );
                // The address the read path publishes, which the host member reports too.
                let url = harness
                    .environment(region, name)
                    .await
                    .get("WebserverUrl")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| panic!("case {row:?}: GetEnvironment reported no WebserverUrl"));
                assert_eq!(
                    url, published,
                    "case {row:?}: expected {published:?}, got {url:?}"
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
                        Some(published),
                        "case {row:?}/{operation}: expected the address the record publishes \
                         ({published:?}), got {reported:?}"
                    );
                    for suffix in AWS_ENDPOINT_SUFFIXES {
                        assert!(
                            !published.ends_with(suffix),
                            "case {row:?}/{operation}: a published host must not name an \
                             endpoint zone of AWS's ({suffix}), got {published:?}"
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn the_published_address_is_one_this_emulator_answers_requests_addressed_to() {
    for (case, endpoint, published) in endpoint_cases() {
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
            hostname, published,
            "case {case:?}: expected {published:?}, got {hostname:?}"
        );

        // The address handed to the caller is one this emulator answers on: a request
        // addressed to that very address reaches this service's environment, rather than
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
            "case {case:?}: a request addressed to the published address {hostname:?} must be \
             answered by this emulator, got {} {}",
            answer.status,
            answer.text()
        );
    }
}
