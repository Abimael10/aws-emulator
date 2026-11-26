// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the MicroVM GUEST data plane: what a client reads back from the
//! `endpoint` `RunMicrovm` publishes, and which outbound connection the MicroVM's
//! `egressNetworkConnectors` carry.
//!
//! Each test states a RULE and exercises it over a SET of inputs; the addresses, ports
//! and ids any one row carries are examples, not the specification. The emulator here
//! mounts the owners of everything the decision reads on ONE engine — this service, the
//! object store the guest's code artifact is staged in, Amazon EC2 (the subnet, the
//! destination interface and its security groups) and lambda-core (the connector) — so
//! every answer is computed from records those services really hold.
//!
//! The guest's program is a Node-shaped HTTP server written here and packaged into the
//! .zip a client uploads: it answers `/health` and, for `/connect?host=&port=`, opens ONE
//! outbound connection and reports what it observed of it. Nothing in the assertions
//! below is a value this emulator chooses — each is what that program computed.

use std::io::Write;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_endpoint::{
    serve_request, AUTH_TOKEN_HEADER, NO_GUEST_RESPONSE_STATUS,
};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_network::connector_state;
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::ComAmazonawsLambdamicrovmsLambdamicrovmsState as MicrovmState;
use smithy_server_runtime::MultiServiceRouter;

use crate::microvm_lifecycle::{call, image_body, run_microvm, REGION};

/// The VPC the connector reaches into, and the subnet its interfaces live in.
const VPC_CIDR: &str = "10.90.0.0/16";

/// The subnet the connector is configured with.
const SUBNET_CIDR: &str = "10.90.1.0/24";

/// The address the destination interface holds in that subnet.
const TARGET_ADDRESS: &str = "10.90.1.4";

/// The port the destination's security group admits.
const ADMITTED_PORT: i32 = 443;

/// The link-local block a guest's own source address comes out of (RFC 3927), which is
/// what the connector-carried path's local end must be inside of.
const LINK_LOCAL_PREFIX: (u8, u8) = (169, 254);

/// The AWS-managed internet-egress connector, in the ARN form an AWS-owned connector of
/// lambda's own namespace carries.
fn internet_egress_arn() -> String {
    format!("arn:aws:lambda:{REGION}:aws:network-connector:aws-network-connector:INTERNET_EGRESS")
}

/// An operator role for the connector: `NetworkConnectorRoleArn`, pattern
/// `^arn:(aws[a-zA-Z-]*)?:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`.
const OPERATOR_ROLE: &str = "arn:aws:iam::123456789012:role/connector-operator";

/// How long the whole poll for one clock-owed convergence may take before the test gives
/// up: generous, because what is asserted is that the transition HAPPENS, not when.
const SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How long one poll of a settling resource waits before looking again.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// The guest program this suite runs: a Node-shaped HTTP server that answers `/health`
/// and reports ONE outbound connection for `/connect?host=&port=`.
const GUEST_SERVER: &str = r#"
const http = require('http');
const net = require('net');
const url = require('url');

http.createServer(function (req, res) {
    const parsed = url.parse(req.url, true);
    const answer = function (status, body) {
        res.writeHead(status, {'content-type': 'application/json'});
        res.end(JSON.stringify(body));
    };
    if (parsed.pathname === '/health') {
        answer(200, {healthy: true});
        return;
    }
    if (parsed.pathname !== '/connect') {
        answer(404, {error: 'no such path'});
        return;
    }
    const host = parsed.query.host;
    const port = Number(parsed.query.port);
    const socket = net.connect({host: host, port: port});
    let reported = false;
    const report = function (outcome) {
        if (reported) { return; }
        reported = true;
        const body = {connected: outcome === 'connected', outcome: outcome, peer: [host, port]};
        if (outcome === 'connected') {
            body.local = [socket.localAddress, socket.localPort];
        }
        answer(200, body);
    };
    socket.setTimeout(5000, function () { socket.destroy(); report('timed_out'); });
    socket.on('connect', function () { report('connected'); socket.destroy(); });
    socket.on('error', function () { report('timed_out'); });
}).listen(8080);
"#;

/// A router carrying this service and every owner of what a guest's connection is decided
/// against, over ONE shared engine.
pub(crate) fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Verifying signatures is the emulator's concern, not this service's; these tests
    // sign nothing, so the check is off and the scope header is read for routing only.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambdamicrovms_lambdamicrovms::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_lambdacore_lambdacoreapiservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// The in-process handle every SDK client below reaches this emulator over.
fn callback(router: &MultiServiceRouter) -> smithy_server_runtime::EmbeddedEmulator {
    smithy_server_runtime::EmbeddedEmulator::from_router(router.clone())
}

/// An Amazon S3 client over this emulator's own in-process callback.
fn s3(router: &MultiServiceRouter) -> aws_sdk_s3::Client {
    let callback = callback(router);
    let config = aws_sdk_s3::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new(REGION))
        .endpoint_url(format!("https://s3.{REGION}.amazonaws.com"))
        .force_path_style(true)
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_s3::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_s3::config::IdentityCache::no_cache())
        .stalled_stream_protection(aws_sdk_s3::config::StalledStreamProtectionConfig::disabled())
        .build();
    aws_sdk_s3::Client::from_conf(config)
}

/// An Amazon EC2 client over the same callback.
fn ec2(router: &MultiServiceRouter) -> aws_sdk_ec2::Client {
    let callback = callback(router);
    let config = aws_sdk_ec2::Config::builder()
        .behavior_version(aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(aws_sdk_ec2::config::Region::new(REGION))
        .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    aws_sdk_ec2::Client::from_conf(config)
}

/// The .zip a client uploads as a MicroVM image's code artifact: the container
/// specification that declares which module starts the guest, and the guest itself.
pub(crate) fn guest_artifact(server: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        for (name, source) in [
            (
                "Dockerfile",
                "FROM node:24-alpine\nCOPY . /app\nWORKDIR /app\nCMD [\"node\", \"server.js\"]\n",
            ),
            ("server.js", server),
        ] {
            writer.start_file(name, options).expect("the entry starts");
            writer
                .write_all(source.as_bytes())
                .expect("the entry's bytes");
        }
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Stage `bytes` as the object `key` of `bucket`, through Amazon S3's own operations.
pub(crate) async fn stage_artifact(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    bytes: Vec<u8>,
) {
    let client = s3(router);
    let created = client
        .create_bucket()
        .bucket(bucket)
        .create_bucket_configuration(
            aws_sdk_s3::types::CreateBucketConfiguration::builder()
                .location_constraint(aws_sdk_s3::types::BucketLocationConstraint::from(REGION))
                .build(),
        )
        .send()
        .await;
    assert!(
        created.is_ok(),
        "staging {bucket}: the bucket must be created, got {created:?}"
    );
    let put = client
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(aws_sdk_s3::primitives::ByteStream::from(bytes))
        .send()
        .await;
    assert!(
        put.is_ok(),
        "staging s3://{bucket}/{key}: the object must be stored, got {put:?}"
    );
}

/// The network the connector reaches into: a VPC, one subnet, the connector's own
/// security group, and a destination interface in that subnet carrying a group of its
/// own.
struct Network {
    subnet_id: String,
    connector_group: String,
    target_group: String,
    target_address: String,
}

/// Create that network through Amazon EC2's own operations.
async fn network(router: &MultiServiceRouter) -> Network {
    let client = ec2(router);
    let vpc = client
        .create_vpc()
        .cidr_block(VPC_CIDR)
        .send()
        .await
        .expect("the VPC is created");
    let vpc_id = vpc
        .vpc()
        .and_then(|vpc| vpc.vpc_id())
        .expect("the VPC carries an id")
        .to_string();
    let subnet = client
        .create_subnet()
        .vpc_id(&vpc_id)
        .cidr_block(SUBNET_CIDR)
        .send()
        .await
        .expect("the subnet is created");
    let subnet_id = subnet
        .subnet()
        .and_then(|subnet| subnet.subnet_id())
        .expect("the subnet carries an id")
        .to_string();

    let mut groups = Vec::new();
    for name in ["microvm-connector", "private-endpoint"] {
        let group = client
            .create_security_group()
            .group_name(name)
            .description(format!("{name} group"))
            .vpc_id(&vpc_id)
            .send()
            .await
            .unwrap_or_else(|error| panic!("the security group {name} is created: {error:?}"));
        groups.push(
            group
                .group_id()
                .expect("the security group carries an id")
                .to_string(),
        );
    }
    let (connector_group, target_group) = (groups[0].clone(), groups[1].clone());

    let interface = client
        .create_network_interface()
        .subnet_id(&subnet_id)
        .groups(&target_group)
        .private_ip_address(TARGET_ADDRESS)
        .description("the private destination this suite connects to")
        .send()
        .await
        .expect("the destination interface is created");
    let target_address = interface
        .network_interface()
        .and_then(|interface| interface.private_ip_address())
        .expect("the interface carries a private address")
        .to_string();

    Network {
        subnet_id,
        connector_group,
        target_group,
        target_address,
    }
}

/// Authorize, or revoke, TCP `port` on `group` from the VPC's own range — the ingress
/// rule that admits the connector's path at the destination.
async fn ingress(router: &MultiServiceRouter, group: &str, port: i32, admit: bool) {
    let client = ec2(router);
    let rule = aws_sdk_ec2::types::IpPermission::builder()
        .ip_protocol("tcp")
        .from_port(port)
        .to_port(port)
        .ip_ranges(
            aws_sdk_ec2::types::IpRange::builder()
                .cidr_ip(VPC_CIDR)
                .build(),
        )
        .build();
    if admit {
        let authorized = client
            .authorize_security_group_ingress()
            .group_id(group)
            .ip_permissions(rule)
            .send()
            .await;
        assert!(
            authorized.is_ok(),
            "admitting tcp/{port} on {group}: got {authorized:?}"
        );
        return;
    }
    let revoked = client
        .revoke_security_group_ingress()
        .group_id(group)
        .ip_permissions(rule)
        .send()
        .await;
    assert!(
        revoked.is_ok(),
        "revoking tcp/{port} on {group}: got {revoked:?}"
    );
}

/// Create a VPC-egress network connector over lambda-core's own operation and poll it
/// until it reports `ACTIVE`, then answer its ARN.
async fn active_connector(router: &MultiServiceRouter, network: &Network) -> String {
    let (status, created) = call(
        router,
        "POST",
        "/2026-04-04/network-connectors",
        Some(json!({
            "Name": "microvm-vpc-egress",
            "OperatorRole": OPERATOR_ROLE,
            "Configuration": {
                "VpcEgressConfiguration": {
                    "SubnetIds": [network.subnet_id],
                    "SecurityGroupIds": [network.connector_group],
                    "NetworkProtocol": "IPv4",
                    "AssociatedComputeResourceTypes": ["MicroVm"],
                }
            },
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "create the network connector: got {status} {created}"
    );
    let arn = created["Arn"]
        .as_str()
        .expect("the connector carries an ARN")
        .to_string();

    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = call(
            router,
            "GET",
            &format!("/2026-04-04/network-connectors/{}", connector_id(&arn)),
            None,
        )
        .await;
        assert_eq!(status, 200, "read the connector: got {status} {read}");
        if read["State"].as_str() == Some("ACTIVE") {
            return arn;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the connector never reached ACTIVE: last read {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The connector id an ARN names — its last segment.
fn connector_id(arn: &str) -> &str {
    arn.rsplit(':').next().unwrap_or(arn)
}

/// Create the image whose code artifact is the staged guest, and poll it until its build
/// has settled with an active version.
async fn guest_image(router: &MultiServiceRouter, name: &str, uri: &str) -> String {
    let (status, created) = call(
        router,
        "POST",
        "/2025-09-09/microvm-images",
        Some(image_body(name, json!({"codeArtifact": {"uri": uri}}))),
    )
    .await;
    assert_eq!(status, 201, "create the image {name}: got {status} {created}");
    let image_arn = created["imageArn"]
        .as_str()
        .expect("the image carries an ARN")
        .to_string();

    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = call(
            router,
            "GET",
            &format!("/2025-09-09/microvm-images/{name}"),
            None,
        )
        .await;
        assert_eq!(status, 200, "read the image {name}: got {status} {read}");
        if read["latestActiveImageVersion"].is_string() {
            return image_arn;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the image {name} never settled with an active version: last read {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run a MicroVM from `image` attached to `egress`, poll it until it reports `RUNNING`,
/// and answer its id and published endpoint.
async fn running_microvm(
    router: &MultiServiceRouter,
    image: &str,
    egress: &[String],
) -> (String, String) {
    let (status, run) = run_microvm(
        router,
        json!({
            "imageIdentifier": image,
            "egressNetworkConnectors": egress,
        }),
    )
    .await;
    assert_eq!(status, 200, "run a MicroVM: got {status} {run}");
    let microvm_id = run["microvmId"]
        .as_str()
        .expect("the MicroVM carries an id")
        .to_string();
    let endpoint = run["endpoint"]
        .as_str()
        .expect("the MicroVM carries an endpoint")
        .to_string();

    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = call(
            router,
            "GET",
            &format!("/2025-09-09/microvms/{microvm_id}"),
            None,
        )
        .await;
        assert_eq!(status, 200, "read the MicroVM: got {status} {read}");
        if read["state"].as_str() == Some("RUNNING") {
            return (microvm_id, endpoint);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the MicroVM {microvm_id} never reached RUNNING: last read {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Mint an authentication token for every port of `microvm_id`.
async fn auth_token(router: &MultiServiceRouter, microvm_id: &str) -> String {
    let (status, minted) = call(
        router,
        "POST",
        &format!("/2025-09-09/microvms/{microvm_id}/auth-token"),
        Some(json!({"expirationInMinutes": 15, "allowedPorts": [{"allPorts": {}}]})),
    )
    .await;
    assert_eq!(status, 200, "mint a token: got {status} {minted}");
    minted["authToken"][AUTH_TOKEN_HEADER]
        .as_str()
        .expect("the token carries the proxy header's value")
        .to_string()
}

/// One authenticated request to a MicroVM's published endpoint: the status and the body
/// the GUEST answered.
///
/// `microvm_id` is the MicroVM the address belongs to: its published endpoint is that
/// address's authority, so `path` is the guest's own path in full.
async fn through_endpoint(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    microvm_id: &str,
    path: &str,
    token: &str,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header(AUTH_TOKEN_HEADER, token)
        .body(Vec::new())
        .expect("a well-formed request to a MicroVM endpoint");
    let state = engine.state::<MicrovmState>();
    let connectors = connector_state(&**engine);
    let response =
        serve_request(&state, connectors.as_ref(), &callback(router), microvm_id, &request)
            .await;
    let status = response.status().as_u16();
    let body = serde_json::from_slice(response.body()).unwrap_or_else(|_| {
        // A body that is not the guest's own JSON — the proxy's own sentence for a MicroVM it
        // reached nothing inside of — is carried as the text it is, so an assertion that
        // fails on it names what the endpoint answered.
        Value::String(String::from_utf8_lossy(response.body()).to_string())
    });
    (status, body)
}

/// Everything the suites below stand up once: the network, the connector, the staged
/// guest and the two MicroVMs it runs.
struct Standing {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    network: Network,
    /// The MicroVM attached to the ACTIVE VPC-egress connector.
    connected: (String, String),
    /// The MicroVM attached only to the AWS-managed internet-egress connector.
    internet_only: (String, String),
}

/// Stand that up.
async fn standing() -> Standing {
    let (router, engine) = emulator();
    let network = network(&router).await;
    ingress(&router, &network.target_group, ADMITTED_PORT, true).await;
    let connector = active_connector(&router, &network).await;
    stage_artifact(
        &router,
        "mvm-guest-artifacts",
        "guest.zip",
        guest_artifact(GUEST_SERVER),
    )
    .await;
    let image = guest_image(
        &router,
        "guest-serving",
        "s3://mvm-guest-artifacts/guest.zip",
    )
    .await;
    let connected = running_microvm(&router, &image, &[connector]).await;
    let internet_only = running_microvm(&router, &image, &[internet_egress_arn()]).await;
    Standing {
        router,
        engine,
        network,
        connected,
        internet_only,
    }
}

/// The body a `/connect` of `address`:`port` reported, through `microvm`.
async fn guest_connect(
    standing: &Standing,
    microvm: &(String, String),
    address: &str,
    port: i32,
) -> Value {
    let token = auth_token(&standing.router, &microvm.0).await;
    let (status, body) = through_endpoint(
        &standing.router,
        &standing.engine,
        &microvm.0,
        &format!("/connect?host={address}&port={port}"),
        &token,
    )
    .await;
    assert_eq!(
        status, 200,
        "GET /connect?host={address}&port={port} on {}: expected the guest's own answer, \
         got {status} {body}",
        microvm.0
    );
    body
}

/// One row of the guest-answer table: the case, the MicroVM asked, the guest path, and
/// the status and body the guest's own program must answer with.
type AnswerRow<'a> = (&'a str, &'a (String, String), &'a str, u16, Value);

/// RULE: an authenticated request to the published `endpoint` of a RUNNING MicroVM is
/// answered with the GUEST's own HTTP response — the status, the content type and the
/// body its program produced — for every path the guest serves and for every MicroVM
/// running that image.
#[tokio::test(flavor = "multi_thread")]
async fn a_published_endpoint_answers_with_the_guests_own_response() {
    let standing = standing().await;

    // The rows differ in WHICH MicroVM is asked and which of the guest's own paths is
    // asked for, including one the guest answers with a status of its own.
    let rows: Vec<AnswerRow> = vec![
        (
            "the health path of the connector-attached MicroVM",
            &standing.connected,
            "/health",
            200,
            json!({"healthy": true}),
        ),
        (
            "the health path of the internet-egress MicroVM",
            &standing.internet_only,
            "/health",
            200,
            json!({"healthy": true}),
        ),
        (
            "a path the guest answers with its own 404",
            &standing.connected,
            "/nothing-here",
            404,
            json!({"error": "no such path"}),
        ),
        (
            "the health path with a query the guest ignores",
            &standing.connected,
            "/health?probe=1",
            200,
            json!({"healthy": true}),
        ),
    ];

    for (case, microvm, path, status, expected) in rows {
        let token = auth_token(&standing.router, &microvm.0).await;
        let (actual_status, actual) = through_endpoint(
            &standing.router,
            &standing.engine,
            &microvm.0,
            path,
            &token,
        )
        .await;
        assert_eq!(
            actual_status, status,
            "case {case:?}: expected the guest's status {status}, got {actual_status} {actual}"
        );
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the guest's body {expected}, got {actual}"
        );
    }
}

/// One row of the reachability table: the case, the MicroVM asked, the destination, the
/// port, and whether the connection is carried.
type ConnectRow<'a> = (&'a str, &'a (String, String), String, i32, bool);

/// RULE: a MicroVM's outbound connection is carried only by an attached ACTIVE VPC-egress
/// connector whose VPC holds the destination and whose path the destination's security
/// groups admit on that port; every other destination is unreached. The guest observes
/// the carried one as established, with a LINK-LOCAL local address rather than one inside
/// the connector's subnet.
#[tokio::test(flavor = "multi_thread")]
async fn only_an_attached_connector_carries_a_guests_connection() {
    let standing = standing().await;
    let target = standing.network.target_address.clone();

    // (case, the MicroVM asked, the destination, the port, whether it connects)
    let rows: Vec<ConnectRow> = vec![
        (
            "the connector's own VPC, on the admitted port",
            &standing.connected,
            target.clone(),
            ADMITTED_PORT,
            true,
        ),
        (
            "the same destination on a port nothing admits",
            &standing.connected,
            target.clone(),
            9_999,
            false,
        ),
        (
            "an address of the connector's subnet nothing holds",
            &standing.connected,
            "10.90.1.200".to_string(),
            ADMITTED_PORT,
            false,
        ),
        (
            "an address outside the connector's VPC",
            &standing.connected,
            "10.99.1.4".to_string(),
            ADMITTED_PORT,
            false,
        ),
        (
            "a public address this account holds no resource at",
            &standing.connected,
            "203.0.113.10".to_string(),
            ADMITTED_PORT,
            false,
        ),
        (
            "a name rather than an address",
            &standing.connected,
            "endpoint.example.internal".to_string(),
            ADMITTED_PORT,
            false,
        ),
        (
            "the same destination from a MicroVM attached only to internet egress",
            &standing.internet_only,
            target.clone(),
            ADMITTED_PORT,
            false,
        ),
    ];

    for (case, microvm, address, port, connects) in rows {
        let body = guest_connect(&standing, microvm, &address, port).await;
        assert_eq!(
            body["connected"].as_bool(),
            Some(connects),
            "case {case:?}: expected connected={connects}, got {body}"
        );
        let expected_outcome = if connects { "connected" } else { "timed_out" };
        assert_eq!(
            body["outcome"].as_str(),
            Some(expected_outcome),
            "case {case:?}: expected outcome {expected_outcome:?}, got {body}"
        );
        assert_eq!(
            body["peer"],
            json!([address, port]),
            "case {case:?}: the guest reports the peer it was asked for, got {body}"
        );
        if !connects {
            assert!(
                body["local"].is_null(),
                "case {case:?}: an unreached connection has no local end, got {body}"
            );
            continue;
        }
        let local = body["local"][0]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a carried connection has a local end: {body}"))
            .to_string();
        let octets: Vec<u8> = local
            .split('.')
            .filter_map(|octet| octet.parse::<u8>().ok())
            .collect();
        assert_eq!(
            (octets.first().copied(), octets.get(1).copied()),
            (Some(LINK_LOCAL_PREFIX.0), Some(LINK_LOCAL_PREFIX.1)),
            "case {case:?}: expected a link-local local address, got {local:?}"
        );
        assert!(
            !local.starts_with("10.90."),
            "case {case:?}: the local end is never an address of the connector's subnet, \
             got {local:?}"
        );
        let port = body["local"][1].as_i64().unwrap_or_default();
        assert!(
            (49_152..=65_535).contains(&port),
            "case {case:?}: expected an ephemeral local port, got {port}"
        );
    }
}

/// RULE: the destination's security group GATES the connector's path, so the same
/// request turns from unreached to established and back as the ingress rule admitting
/// the port is authorized and revoked.
#[tokio::test(flavor = "multi_thread")]
async fn the_destinations_security_group_gates_the_connectors_path() {
    let standing = standing().await;
    let target = standing.network.target_address.clone();
    let group = standing.network.target_group.clone();

    // The rows are the sequence the rule is read over: the control, the revocation, the
    // re-authorization — each asserted on the SAME request.
    let rows: Vec<(&str, Option<bool>, bool)> = vec![
        ("the rule the suite admitted", None, true),
        ("the rule revoked", Some(false), false),
        ("the rule authorized again", Some(true), true),
    ];

    for (case, change, connects) in rows {
        if let Some(admit) = change {
            ingress(&standing.router, &group, ADMITTED_PORT, admit).await;
        }
        let body = guest_connect(&standing, &standing.connected, &target, ADMITTED_PORT).await;
        assert_eq!(
            body["connected"].as_bool(),
            Some(connects),
            "case {case:?}: expected connected={connects}, got {body}"
        );
    }
}

/// The PYTHON guest the suites below run: an `http.server` program that answers `/health`,
/// reports ONE outbound connection for `/connect?host=&port=`, and reports what each attempt
/// to reach the HOST raised for `/escape?what=`.
///
/// Nothing in the assertions below is a value this emulator chooses — each is what this
/// program computed inside its MicroVM.
const PYTHON_GUEST: &str = r#"
import errno
import json
import socket
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer


def _walk(root, seen=None):
    """Every type reachable from `root` by subclass, which is how a type nothing names any
    more is still reached."""
    if seen is None:
        seen = set()
    for kind in root.__subclasses__():
        if id(kind) in seen:
            continue
        seen.add(id(kind))
        yield kind
        for held in _walk(kind, seen):
            yield held


def attempt(what, path):
    """What reaching the host this way raised, or "allowed" when nothing did."""
    try:
        if what == "host_file":
            open(path).read()
        elif what == "io_open":
            import io

            io.open(path).read()
        elif what == "raw_file":
            import _io

            _io.FileIO(path, "r").read()
        elif what == "traverse":
            # The type behind a host file, reached by WALKING the type graph rather than by
            # the name it was taken off: a type this interpreter registered would still be
            # one `object.__subclasses__()` away.
            reached = [kind for kind in _walk(object) if kind.__name__ == "FileIO"]
            if not reached:
                return "no such type"
            for kind in reached:
                kind(path, "r").read()
        elif what == "socket_extension":
            import _socket
        elif what == "listen":
            socket.socket().bind(("0.0.0.0", 9000))
        elif what == "thread":
            import _thread

            _thread.start_new_thread(lambda: None, ())
        elif what == "subprocess":
            import subprocess

            subprocess.run(["/bin/sh", "-c", "id"])
        elif what == "native_library":
            import ctypes
        elif what == "untrace":
            sys.settrace(None)
        elif what == "recursion":
            sys.setrecursionlimit(10 ** 6)
        elif what == "builtin_module":
            import _imp

            _imp.create_builtin("posix")
        else:
            return "no such attempt"
    except BaseException as raised:
        return type(raised).__name__
    return "allowed"


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        query = urllib.parse.parse_qs(parsed.query)
        if parsed.path == "/health":
            self.answer(200, {"healthy": True})
            return
        if parsed.path == "/escape":
            what = query.get("what", [""])[0]
            path = query.get("path", [""])[0]
            self.answer(
                200,
                {
                    "what": what,
                    "raised": attempt(what, path),
                    "builtins": sorted(sys.builtin_module_names),
                },
            )
            return
        if parsed.path == "/spin":
            while True:
                pass
        if parsed.path == "/recurse":
            self.answer(200, self.recursed())
            return
        if parsed.path != "/connect":
            self.answer(404, {"error": "no such path"})
            return
        host = query.get("host", [""])[0]
        port = int(query.get("port", ["0"])[0])
        self.answer(200, self.connected(host, port))

    def recursed(self):
        """How deep this guest got before the platform stopped it recursing."""
        depth = [0]

        def descend():
            depth[0] += 1
            descend()

        try:
            descend()
        except RecursionError:
            return {"depth": depth[0], "raised": "RecursionError"}
        return {"depth": depth[0], "raised": "none"}

    def connected(self, host, port):
        try:
            opened = socket.create_connection((host, port), timeout=8)
        except TimeoutError as raised:
            return {
                "connected": False,
                "outcome": "timed_out",
                "peer": [host, port],
                "detail": str(raised)[:200],
            }
        except ConnectionRefusedError:
            return {"connected": False, "outcome": "refused", "peer": [host, port]}
        except OSError as raised:
            unreachable = (errno.EHOSTUNREACH, errno.ENETUNREACH)
            outcome = (
                "unreachable"
                if raised.errno in unreachable
                else "error:" + type(raised).__name__
            )
            return {"connected": False, "outcome": outcome, "peer": [host, port]}
        try:
            return {
                "connected": True,
                "outcome": "connected",
                "peer": list(opened.getpeername()),
                "local": list(opened.getsockname()),
            }
        finally:
            opened.close()

    def answer(self, status, body):
        payload = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


if __name__ == "__main__":
    HTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
"#;

/// The container specification that declares a PYTHON guest the way a client's build
/// context does: the interpreter is installed, the module is written by a `RUN` step, and
/// the command starts it.
pub(crate) fn python_specification(server: &str) -> String {
    let encoded = aws_smithy_types::base64::encode(server);
    format!(
        "FROM public.ecr.aws/amazonlinux/amazonlinux:2023\n\
         RUN dnf install -y python3 && dnf clean all\n\
         RUN mkdir -p /srv\n\
         RUN echo {encoded} | base64 -d > /srv/app.py\n\
         CMD [\"/bin/sh\", \"-c\", \"python3 /srv/app.py\"]\n"
    )
}

/// The .zip a client uploads whose Python guest is an ENTRY of the archive rather than a
/// file a build step writes — the other form the same image is staged in.
pub(crate) fn python_entry_artifact(server: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        for (name, source) in [
            (
                "Dockerfile",
                "FROM public.ecr.aws/amazonlinux/amazonlinux:2023\nCOPY . /srv\n\
                 CMD [\"python3\", \"/srv/app.py\"]\n"
                    .to_string(),
            ),
            ("srv/app.py", server.to_string()),
        ] {
            writer.start_file(name, options).expect("the entry starts");
            writer
                .write_all(source.as_bytes())
                .expect("the entry's bytes");
        }
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Everything the Python suites stand up once: the network, the ACTIVE connector, the
/// staged Python guest and the two MicroVMs it runs.
async fn python_standing() -> Standing {
    let (router, engine) = emulator();
    let network = network(&router).await;
    ingress(&router, &network.target_group, ADMITTED_PORT, true).await;
    let connector = active_connector(&router, &network).await;
    stage_artifact(
        &router,
        "mvm-python-artifacts",
        "guest.zip",
        specification_only_artifact(&python_specification(PYTHON_GUEST)),
    )
    .await;
    let image = guest_image(
        &router,
        "python-guest-serving",
        "s3://mvm-python-artifacts/guest.zip",
    )
    .await;
    let connected = running_microvm(&router, &image, &[connector]).await;
    let internet_only = running_microvm(&router, &image, &[internet_egress_arn()]).await;
    Standing {
        router,
        engine,
        network,
        connected,
        internet_only,
    }
}

/// RULE: a MicroVM whose image declares a PYTHON guest runs that program, so an
/// authenticated request to the published endpoint is answered with the response the
/// program itself produced — whichever form the image was staged in, and whichever of its
/// own paths is asked for.
///
/// The rows differ along the form the image is staged in (a guest a build step wrote, a
/// guest the archive carries as an entry) and, within each, along the path asked for and the
/// status the guest's own program answers with.
#[tokio::test(flavor = "multi_thread")]
async fn a_python_guests_own_response_is_what_the_endpoint_answers() {
    // (the case of this staging form, the artifact staged)
    let forms: Vec<(&str, Vec<u8>)> = vec![
        (
            "a guest a build step wrote",
            specification_only_artifact(&python_specification(PYTHON_GUEST)),
        ),
        (
            "a guest the archive carries as an entry",
            python_entry_artifact(PYTHON_GUEST),
        ),
    ];
    // (the path asked for, the status and body the guest answers it with)
    let paths: Vec<(&str, u16, Value)> = vec![
        ("/health", 200, json!({"healthy": true})),
        ("/health?probe=1", 200, json!({"healthy": true})),
        ("/nothing-here", 404, json!({"error": "no such path"})),
    ];

    for (form, artifact) in forms {
        let (router, engine) = emulator();
        stage_artifact(&router, "mvm-python-forms", "guest.zip", artifact).await;
        let image = guest_image(
            &router,
            "python-guest-form",
            "s3://mvm-python-forms/guest.zip",
        )
        .await;
        let microvm = running_microvm(&router, &image, &[internet_egress_arn()]).await;
        let token = auth_token(&router, &microvm.0).await;
        for (path, status, expected) in &paths {
            let (actual_status, actual) =
                through_endpoint(&router, &engine, &microvm.0, path, &token).await;
            assert_eq!(
                actual_status, *status,
                "case {form:?} on {path:?}: expected the guest's status {status}, got \
                 {actual_status} {actual}"
            );
            assert_eq!(
                actual, *expected,
                "case {form:?} on {path:?}: expected the guest's body {expected}, got {actual}"
            );
        }
    }
}

/// RULE: a PYTHON guest's outbound connection is decided by exactly what a JavaScript
/// guest's is — only an attached ACTIVE VPC-egress connector whose VPC holds the
/// destination and whose path the destination's security groups admit on that port carries
/// one — and the guest observes the carried one as established with a LINK-LOCAL local
/// address, and an uncarried one as the timeout its own `socket` call raised.
#[tokio::test(flavor = "multi_thread")]
async fn an_attached_connector_decides_a_python_guests_connection() {
    let standing = python_standing().await;
    let target = standing.network.target_address.clone();

    // (case, the MicroVM asked, the destination, the port, whether it connects)
    let rows: Vec<ConnectRow> = vec![
        (
            "the connector's own VPC, on the admitted port",
            &standing.connected,
            target.clone(),
            ADMITTED_PORT,
            true,
        ),
        (
            "the same destination on a port nothing admits",
            &standing.connected,
            target.clone(),
            9_999,
            false,
        ),
        (
            "an address of the connector's subnet nothing holds",
            &standing.connected,
            "10.90.1.200".to_string(),
            ADMITTED_PORT,
            false,
        ),
        (
            "an address outside the connector's VPC",
            &standing.connected,
            "10.99.1.4".to_string(),
            ADMITTED_PORT,
            false,
        ),
        (
            "the same destination from a MicroVM attached only to internet egress",
            &standing.internet_only,
            target.clone(),
            ADMITTED_PORT,
            false,
        ),
    ];

    for (case, microvm, address, port, connects) in rows {
        let body = guest_connect(&standing, microvm, &address, port).await;
        assert_eq!(
            body["connected"].as_bool(),
            Some(connects),
            "case {case:?}: expected connected={connects}, got {body}"
        );
        let expected_outcome = if connects { "connected" } else { "timed_out" };
        assert_eq!(
            body["outcome"].as_str(),
            Some(expected_outcome),
            "case {case:?}: expected outcome {expected_outcome:?}, got {body}"
        );
        assert_eq!(
            body["peer"],
            json!([address, port]),
            "case {case:?}: the guest reports the peer it was asked for, got {body}"
        );
        if !connects {
            assert!(
                body["local"].is_null(),
                "case {case:?}: an unreached connection has no local end, got {body}"
            );
            continue;
        }
        let local = body["local"][0]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a carried connection has a local end: {body}"))
            .to_string();
        let octets: Vec<u8> = local
            .split('.')
            .filter_map(|octet| octet.parse::<u8>().ok())
            .collect();
        assert_eq!(
            (octets.first().copied(), octets.get(1).copied()),
            (Some(LINK_LOCAL_PREFIX.0), Some(LINK_LOCAL_PREFIX.1)),
            "case {case:?}: expected a link-local local address, got {local:?}"
        );
    }
}

/// ONE MicroVM running the Python guest, on the AWS-managed internet-egress connector and
/// with no VPC of its own: what a suite that reads the GUEST rather than the network it
/// reaches needs, and one live guest process rather than the two a connector-attached
/// standing holds.
async fn python_microvm() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>, String, String) {
    let (router, engine) = emulator();
    stage_artifact(
        &router,
        "mvm-python-guest",
        "guest.zip",
        specification_only_artifact(&python_specification(PYTHON_GUEST)),
    )
    .await;
    let image = guest_image(&router, "python-guest", "s3://mvm-python-guest/guest.zip").await;
    let microvm = running_microvm(&router, &image, &[internet_egress_arn()]).await;
    let token = auth_token(&router, &microvm.0).await;
    (router, engine, microvm.0, token)
}

/// RULE: nothing of the HOST is reachable from a MicroVM's Python guest, and no bound of
/// its run can be switched off from inside it — every attempt raises, and the names the
/// interpreter reports as built in carry no module whose calls land on the host.
///
/// The rows are one attempt each, from GUEST code, over the surfaces a program would reach
/// the host through: a file, the stream type behind one, a socket extension, a listening
/// socket, a thread, a child process, a native library, the trace slot, the recursion
/// limit and the creation of a built-in module.
#[tokio::test(flavor = "multi_thread")]
async fn nothing_of_the_host_is_reachable_from_a_python_guest() {
    let (router, engine, microvm, token) = python_microvm().await;
    // A file this process itself can read, which the MicroVM's own machine holds nothing at:
    // what a guest asking for a path of the HOST is asking for.
    let host_file = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");

    // The host-reaching modules a program would name, none of which this interpreter
    // registers (`guest_python::ALLOWED_NATIVE_MODULES` names what it does).
    let unregistered = [
        "_socket", "_ssl", "_posixsubprocess", "mmap", "_ctypes", "fcntl", "_signal",
    ];
    // (the attempt, the error classes a platform answers it with)
    let attempts: Vec<(&str, &[&str])> = vec![
        ("host_file", &["FileNotFoundError", "OSError", "PermissionError"]),
        ("io_open", &["OSError", "PermissionError"]),
        ("raw_file", &["OSError", "PermissionError", "TypeError"]),
        // The traversal row: this interpreter registers no type that opens a host file at
        // all, so the walk from `object` reaches none — and where a build did register one,
        // its construction is the refusal the substrate installs and reads back.
        (
            "traverse",
            &["no such type", "OSError", "PermissionError", "TypeError"],
        ),
        (
            "socket_extension",
            &["ImportError", "ModuleNotFoundError"],
        ),
        ("listen", &["OSError", "PermissionError"]),
        (
            "thread",
            &["RuntimeError", "ImportError", "ModuleNotFoundError"],
        ),
        ("subprocess", &["ImportError", "ModuleNotFoundError"]),
        ("native_library", &["ImportError", "ModuleNotFoundError"]),
        ("untrace", &["AttributeError"]),
        ("recursion", &["AttributeError"]),
        ("builtin_module", &["ImportError", "ModuleNotFoundError"]),
    ];

    for (what, expected) in attempts {
        let (status, body) = through_endpoint(
            &router,
            &engine,
            &microvm,
            &format!("/escape?what={what}&path={host_file}"),
            &token,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {what:?}: the guest answers what the attempt raised, got {status} {body}"
        );
        let raised = body["raised"].as_str().unwrap_or_default();
        assert!(
            expected.contains(&raised),
            "case {what:?}: expected one of {expected:?}, got {raised:?} in {body}"
        );
        let names: Vec<&str> = body["builtins"]
            .as_array()
            .map(|held| held.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for name in unregistered {
            assert!(
                !names.contains(&name),
                "case {what:?}: {name} is no built-in module of this interpreter, got {body}"
            );
        }
    }
}

/// RULE: a request a MicroVM's Python guest never answers is bounded by the HOST — the guest
/// is stopped and the client is answered for a MicroVM nothing was reached inside of — and
/// the bound is spent per REQUEST, so the MicroVM goes on answering afterwards. How deep the
/// guest may recurse is clamped the same way, which its own code observes as the recursion
/// error a platform raises.
#[tokio::test(flavor = "multi_thread")]
async fn a_python_guest_that_never_answers_is_stopped_and_the_microvm_goes_on() {
    let (router, engine, microvm, token) = python_microvm().await;

    // (case, the path asked for, the status the endpoint answers)
    let rows: Vec<(&str, &str, u16)> = vec![
        ("a request the guest answers", "/health", 200),
        (
            "a request the guest never returns from",
            "/spin",
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "the request after it, which has its own budget",
            "/health",
            200,
        ),
    ];

    for (case, path, expected) in rows {
        let (status, body) = through_endpoint(&router, &engine, &microvm, path, &token).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected {expected} from {path}, got {status} {body}"
        );
    }

    let (status, body) =
        through_endpoint(&router, &engine, &microvm, "/recurse", &token).await;
    assert_eq!(
        status, 200,
        "the guest reports what its own recursion came to, got {status} {body}"
    );
    assert_eq!(
        body["raised"].as_str(),
        Some("RecursionError"),
        "a guest that recurses without end reads the platform's own error, got {body}"
    );
    let depth = body["depth"].as_i64().unwrap_or_default();
    assert!(
        depth > 0 && depth < 10_000,
        "the clamp the host installed is what stopped it, got depth {depth} in {body}"
    );
}

/// The .zip a client uploads whose ONLY entry is the container specification: the guest does
/// not exist as an entry of the archive at all — the build's own `RUN` steps materialise it,
/// which is the ordinary container-image shape for this service.
pub(crate) fn specification_only_artifact(dockerfile: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("Dockerfile", options)
            .expect("the entry starts");
        writer
            .write_all(dockerfile.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The guest program the rows below materialise with a build step, in the one-line form a
/// `RUN` writes a file in.
const BAKED_GUEST: &str =
    "require(\"http\").createServer(function (req, res) { res.writeHead(200, \
     {\"content-type\": \"application/json\"}); res.end(JSON.stringify({baked: true, \
     path: req.url})); }).listen(8080);";

/// One row of the baked-guest table: what a failing assertion calls it, the `RUN` steps
/// the specification declares, and the command that starts the guest.
type BakedRow<'a> = (&'a str, Vec<String>, &'a str);

/// RULE: what a MicroVM image's own container specification BUILDS is part of the image,
/// so the module its command names is the guest that runs — whether that module is an
/// entry of the code artifact or a file the build's own steps wrote.
///
/// The rows differ along how the step materialises the guest (a redirection, an appended
/// pair of redirections, a decoded pipeline), where in the image it writes it, and how the
/// command names it.
#[tokio::test]
async fn a_guest_the_builds_own_steps_wrote_is_the_guest_that_runs() {
    // `base64 -d` is the decoder GNU coreutils' `base64` provides, which the shell this
    // emulator performs a build step in carries; the encoded text is that same program,
    // encoded by the codec this workspace already uses.
    let encoded = aws_smithy_types::base64::encode(BAKED_GUEST);
    let rows: Vec<BakedRow> = vec![
        (
            "a step that redirects the program into place",
            vec![format!("echo '{BAKED_GUEST}' > /server.js")],
            "CMD [\"node\", \"/server.js\"]",
        ),
        (
            "a step that makes a directory first, and names the module in shell form",
            vec![
                "mkdir -p /opt".to_string(),
                format!("echo '{BAKED_GUEST}' > /main.js"),
            ],
            "CMD [\"/bin/sh\", \"-c\", \"node /main.js\"]",
        ),
        (
            "a step that decodes the program out of a pipeline",
            vec![format!("echo {encoded} | base64 -d > /decoded.js")],
            "ENTRYPOINT [\"node\", \"/decoded.js\"]",
        ),
    ];

    for (case, steps, command) in rows {
        let (router, engine) = emulator();
        let dockerfile = format!(
            "FROM public.ecr.aws/amazonlinux/amazonlinux:2023\n{}\n{command}\n",
            steps
                .iter()
                .map(|step| format!("RUN {step}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let key = "baked.zip";
        stage_artifact(
            &router,
            "mvm-baked-artifacts",
            key,
            specification_only_artifact(&dockerfile),
        )
        .await;
        let image = guest_image(
            &router,
            "baked-guest",
            &format!("s3://mvm-baked-artifacts/{key}"),
        )
        .await;
        let microvm = running_microvm(&router, &image, &[internet_egress_arn()]).await;
        let token = auth_token(&router, &microvm.0).await;
        let (status, body) =
            through_endpoint(&router, &engine, &microvm.0, "/probe", &token).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the guest's own answer, got {status} {body}"
        );
        assert_eq!(
            body,
            json!({"baked": true, "path": "/probe"}),
            "case {case:?}: expected the body the baked guest wrote, got {body}"
        );
    }
}
