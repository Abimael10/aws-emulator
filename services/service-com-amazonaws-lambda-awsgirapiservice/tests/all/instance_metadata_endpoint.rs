// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the METADATA an execution environment publishes to the handler running
//! in it.
//!
//! RULE: an execution environment publishes its own facts to the handler inside it, on the
//! address and with the token that handler's own environment names — so a read of
//! `AWS_LAMBDA_METADATA_API` presenting `AWS_LAMBDA_METADATA_TOKEN` is answered with the
//! document of that environment (the function it runs, the account that owns it, the Region
//! the invocation was addressed to, an Availability Zone that Region really publishes under
//! the name AND the id Amazon EC2 reports for it, the architecture it runs on and when the
//! environment was initialised), a read presenting no valid token is refused `401` with that
//! status's own reason as its body, and a path the interface does not publish is `404`. The
//! same facts answer the link-local address every node's metadata service answers at, where a
//! PUT of the token path mints a session token and a read presenting it is answered.
//!
//! The variable names, the document path, the `Authorization` header, the document's member
//! names and the refusal body below are that interface's recorded contract, not this
//! implementation's shape: the recording of a handler reading it names each variable in a
//! `KeyError` when the environment does not, `GET`s exactly that path, and its check reads
//! `AvailabilityZoneID` out of the document and asserts the refused read's body.
//!
//! The rows differ along what the rule generalises over: the Region the invocation was
//! addressed to (one with the three zones most Regions publish, one that publishes more), the
//! architecture the function runs on, the token a read presents (the one the environment was
//! handed, one handed to another environment, none at all), the path read, and which of the two
//! addressings the read was made on.

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(200);

/// The path the environment's own document is published at, written out here so the assertion
/// is independent of the constant the implementation carries.
const DOCUMENT_PATH: &str = "/2026-01-15/metadata/execution-environment";

/// The handler every row runs: it reads the address and the token out of its own environment,
/// reads the document WITHOUT presenting the token and then WITH the token the event says to
/// present, reads the same facts off the node's own metadata service, and reports what each
/// request was answered with — so what is asserted is what the environment answered the guest.
const HANDLER_SOURCE: &str = r#"
import os
import urllib.error
import urllib.request

TTL_HEADER = "X-aws-ec2-metadata-token-ttl-seconds"
NODE_TOKEN_HEADER = "X-aws-ec2-metadata-token"


def asked(url, headers=None, method="GET"):
    sent = urllib.request.Request(url, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(sent) as answer:
            return {"status": answer.status, "body": answer.read().decode("utf-8")}
    except urllib.error.HTTPError as error:
        return {"status": error.code, "body": error.read().decode("utf-8")}


def lambda_handler(event, context):
    handed = os.environ["AWS_LAMBDA_METADATA_TOKEN"]
    presented = event.get("token", handed)
    url = "http://" + os.environ["AWS_LAMBDA_METADATA_API"] + event.get(
        "path", "/2026-01-15/metadata/execution-environment"
    )
    out = {
        "token": handed,
        "noauth": asked(url),
        "authed": asked(url, {"Authorization": presented}),
        "region": os.environ.get("AWS_REGION", ""),
    }
    node = "http://" + event["node"]
    minted = asked(node + "/latest/api/token", {TTL_HEADER: "21600"}, "PUT")
    out["node_token_status"] = minted["status"]
    held = {NODE_TOKEN_HEADER: minted["body"]}
    out["node_unauthenticated"] = asked(node + "/latest/meta-data/placement/availability-zone")
    out["node_zone"] = asked(node + "/latest/meta-data/placement/availability-zone", held)
    out["node_zone_id"] = asked(node + "/latest/meta-data/placement/availability-zone-id", held)
    out["node_document"] = asked(node + "/latest/dynamic/instance-identity/document", held)
    out["node_unpublished"] = asked(node + "/latest/meta-data/instance-id", held)
    return out
"#;

/// One dispatched request's answer.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "expected a JSON body, got {error}: {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// Lambda and the Amazon EC2 service that owns the account's Availability Zone catalogue, on
/// ONE shared engine, exactly as the router mounts them.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // Pinned off so the suite does not depend on how the emulator process was started; these
    // tests assert the metadata the environment publishes, not the emulator's SigV4 check.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
}

/// An Amazon EC2 client for `region` over the in-process seam this emulator routes on: no
/// socket is opened and the endpoint URL is only the address that transport routes by.
fn ec2_client(callback: &EmbeddedEmulator, region: &str) -> aws_sdk_ec2::Client {
    let config = aws_sdk_ec2::Config::builder()
        .behavior_version(aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    aws_sdk_ec2::Client::from_conf(config)
}

/// The zones Amazon EC2 itself reports for `region`, as (name, id) pairs: what a client asking
/// that service which zones the account has is told.
async fn zones_of(callback: &EmbeddedEmulator, region: &str) -> Vec<(String, String)> {
    let described = ec2_client(callback, region)
        .describe_availability_zones()
        .send()
        .await
        .expect("DescribeAvailabilityZones must answer");
    described
        .availability_zones()
        .iter()
        .map(|zone| {
            (
                zone.zone_name().unwrap_or_default().to_string(),
                zone.zone_id().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Dispatch one control-plane request in `region`: the Region a request is addressed to is the
/// one its SigV4 credential scope names, and that is the Region the invocation runs in.
/// Nothing in the scope is verified — the harness turns request authentication off.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: &[u8],
) -> Answer {
    let host = format!("lambda.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://{host}{uri}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{region}/lambda/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// A .zip deployment package holding the handler above.
fn package() -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    archive
        .start_file::<_, ()>("lambda_function.py", zip::write::SimpleFileOptions::default())
        .expect("the entry must open");
    archive
        .write_all(HANDLER_SOURCE.as_bytes())
        .expect("the entry must be written");
    archive
        .finish()
        .expect("the archive must finish")
        .into_inner()
}

/// Create the reading function in `region`, on `architecture`, and wait out the provisioning
/// window.
async fn given_function(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    architecture: &str,
) {
    let created = call(
        router,
        region,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Architectures": [architecture],
            "Timeout": 20,
            "MemorySize": 512,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package()),
            },
        }))
        .expect("the request body must serialize"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} in {region} must succeed, got {} {}",
        created.status,
        created.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// Invoke the reading function with `event` and report what the handler returned.
///
/// Every event carries the NODE's own metadata address, built from the one spelling of it this
/// workspace keeps (`service_common::instance_metadata_service::metadata_address`) rather than
/// written out in the handler, so the suite and the emulator cannot disagree about it.
async fn invoked(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    case: &str,
    event: &Value,
) -> Value {
    let mut event = event.clone();
    if let Some(named) = event.as_object_mut() {
        named.insert(
            "node".to_string(),
            Value::String(service_common::instance_metadata_service::metadata_address()),
        );
    }
    let answered = call(
        router,
        region,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &serde_json::to_vec(&event).expect("the event must serialize"),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected the reading invocation to succeed, got {} {}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {case:?}: the handler must return rather than raise, got {}",
        answered.text()
    );
    answered.json()
}

/// The status one of the handler's reads was answered with.
fn status_of(read: &Value, member: &str) -> Option<u64> {
    read.get(member)
        .and_then(|read| read.get("status"))
        .and_then(Value::as_u64)
}

/// The body one of the handler's reads was answered with.
fn body_of(read: &Value, member: &str) -> String {
    read.get(member)
        .and_then(|read| read.get("body"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// RULE: the environment's document is answered to a read presenting the token the environment
/// was handed, out of that environment's own facts, and to no other read.
#[tokio::test]
async fn a_handler_reads_its_execution_environments_own_metadata() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let callback = EmbeddedEmulator::from_router(router.clone());
    // (the Region, the architecture the function runs on, the function's name)
    let environments: &[(&str, &str, &str)] = &[
        // A Region whose published zones are the three most Regions have.
        ("eu-central-1", "x86_64", "metadata-reader-eu"),
        // One that publishes more than three, on the other architecture.
        ("us-west-2", "arm64", "metadata-reader-us"),
    ];
    for (region, architecture, name) in environments {
        given_function(&router, region, name, architecture).await;
        // WHICH zones that Region really has, as the service that owns the catalogue reports
        // them: the environment's placement has to be one of these, under both its names.
        let zones = zones_of(&callback, region).await;
        assert!(
            !zones.is_empty(),
            "case {region:?}: Amazon EC2 must report the Region's zones"
        );

        let read = invoked(
            &router,
            region,
            name,
            &format!("{region}: the environment's own document"),
            &json!({ "path": DOCUMENT_PATH }),
        )
        .await;
        assert_eq!(
            status_of(&read, "noauth"),
            Some(401),
            "case {region:?}: expected a read presenting no token to be refused 401, got {read}"
        );
        assert_eq!(
            body_of(&read, "noauth"),
            "Unauthorized\n",
            "case {region:?}: expected the refusal's own reason as its body, got {read}"
        );
        assert_eq!(
            status_of(&read, "authed"),
            Some(200),
            "case {region:?}: expected the read presenting the token to be answered 200, got \
             {read}"
        );
        assert!(
            read.get("token")
                .and_then(Value::as_str)
                .is_some_and(|token| !token.is_empty()),
            "case {region:?}: expected the environment to be handed a token, got {read}"
        );
        let document: Value = serde_json::from_str(&body_of(&read, "authed")).unwrap_or_else(
            |error| panic!("case {region:?}: expected a JSON document, got {error}"),
        );
        let zone = document
            .get("AvailabilityZone")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let zone_id = document
            .get("AvailabilityZoneID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            zones.contains(&(zone.clone(), zone_id.clone())),
            "case {region:?}: expected a zone Amazon EC2 reports for the Region ({zones:?}), \
             got {zone:?}/{zone_id:?}"
        );
        // (the member, the value this environment's own facts give it)
        let members: &[(&str, &str)] = &[
            ("AccountID", engine::emulator_account_id()),
            ("Architecture", architecture),
            ("FunctionName", name),
            ("FunctionVersion", "$LATEST"),
            ("Handler", "lambda_function.lambda_handler"),
            ("Region", region),
        ];
        for (member, expected) in members {
            let held = document.get(member).and_then(Value::as_str);
            assert_eq!(
                held,
                Some(*expected),
                "case {region:?}, member {member:?}: expected {expected:?}, got {held:?} in \
                 {document}"
            );
        }
        assert_eq!(
            read.get("region").and_then(Value::as_str),
            Some(*region),
            "case {region:?}: expected the environment to run in the Region it was addressed \
             in, got {read}"
        );

        // The NODE's own metadata service answers the same placement, on the link-local
        // address a program on an instance reads: a token is minted by the PUT, a read
        // presenting it is answered and one presenting none is refused.
        assert_eq!(
            read.get("node_token_status").and_then(Value::as_u64),
            Some(200),
            "case {region:?}: expected the node's token request to be answered 200, got {read}"
        );
        assert_eq!(
            status_of(&read, "node_unauthenticated"),
            Some(401),
            "case {region:?}: expected a node read presenting no token to be 401, got {read}"
        );
        assert_eq!(
            body_of(&read, "node_zone"),
            zone,
            "case {region:?}: expected the node to publish the same zone, got {read}"
        );
        assert_eq!(
            body_of(&read, "node_zone_id"),
            zone_id,
            "case {region:?}: expected the node to publish the same zone id, got {read}"
        );
        assert_eq!(
            status_of(&read, "node_unpublished"),
            Some(404),
            "case {region:?}: an execution environment has no instance id to publish, got \
             {read}"
        );
        let identity: Value = serde_json::from_str(&body_of(&read, "node_document"))
            .unwrap_or_else(|error| {
                panic!("case {region:?}: expected a JSON identity document, got {error}")
            });
        assert_eq!(
            identity.get("availabilityZone").and_then(Value::as_str),
            Some(zone.as_str()),
            "case {region:?}: expected the identity document to name the same zone, got \
             {identity}"
        );
        assert_eq!(
            identity.get("region").and_then(Value::as_str),
            Some(*region),
            "case {region:?}: expected the identity document to name the Region, got {identity}"
        );
        assert_eq!(
            identity.get("instanceId"),
            Some(&Value::Null),
            "case {region:?}: an execution environment has no instance id, got {identity}"
        );

        // A path the interface publishes nothing at is answered as that, with that status's
        // own reason as its body.
        let elsewhere = invoked(
            &router,
            region,
            name,
            &format!("{region}: a path the interface does not publish"),
            &json!({ "path": "/2026-01-15/metadata/elsewhere" }),
        )
        .await;
        assert_eq!(
            status_of(&elsewhere, "authed"),
            Some(404),
            "case {region:?}: expected an unpublished path to be 404, got {elsewhere}"
        );
        assert_eq!(
            body_of(&elsewhere, "authed"),
            "Not Found\n",
            "case {region:?}: expected the refusal's own reason as its body, got {elsewhere}"
        );
    }

    // The token is the ENVIRONMENT's: the one an earlier invocation of the same function read
    // out of its environment is still answered, and the one another function's environment was
    // handed is not.
    let (region, architecture, first) = environments[0];
    given_function(&router, region, "metadata-reader-second", architecture).await;
    let earlier = invoked(
        &router,
        region,
        first,
        "the token an earlier invocation was handed",
        &json!({}),
    )
    .await;
    let token = earlier
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let later = invoked(
        &router,
        region,
        first,
        "a later invocation of the same function presenting it",
        &json!({ "token": token.clone() }),
    )
    .await;
    assert_eq!(
        status_of(&later, "authed"),
        Some(200),
        "case \"a later invocation of the same function\": expected the token to still be \
         answered 200, got {later}"
    );
    let elsewhere = invoked(
        &router,
        region,
        "metadata-reader-second",
        "that token presented in another function's environment",
        &json!({ "token": token }),
    )
    .await;
    assert_eq!(
        status_of(&elsewhere, "authed"),
        Some(401),
        "case \"a token presented in another environment\": expected 401, got {elsewhere}"
    );
    let document: Value = serde_json::from_str(&body_of(&elsewhere, "noauth")).unwrap_or(
        Value::Null,
    );
    assert_eq!(
        document,
        Value::Null,
        "case \"a refused read\": expected no document in it, got {document}"
    );
}
