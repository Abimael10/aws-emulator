// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the MicroVM ENDPOINT: the address `RunMicrovm` publishes and
//! what this emulator answers on it.
//!
//! Each test states a RULE and exercises it over a SET of inputs; the values any one
//! row happens to carry are examples, not the specification. The control plane is
//! driven over the wire through the same router the emulator boots (the harness
//! `microvm_lifecycle` owns); the endpoint itself is driven through
//! `state::microvm_endpoint::serve_request`, which is the one decision the core's
//! HTTPS listener hands every request on that address to — the listener's own
//! transport (TLS, CORS, the Host guard) is the core's and is not re-tested here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_endpoint::{
    auth_token_digest, serve_request, AUTH_TOKEN_HEADER, NO_GUEST_RESPONSE_STATUS,
};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_network::connector_state;
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    auth_token_expiration_exceeded_message, microvm_not_found_message,
    ComAmazonawsLambdamicrovmsLambdamicrovmsState as MicrovmState,
    MAX_AUTH_TOKEN_EXPIRATION_MINUTES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::microvm_lifecycle::{
    built_image, call, emulator_with_engine, image_body, run_microvm,
};
use crate::microvm_suspend_resume::{deadline_left, set_deadline, Deadline};

/// One row of the status table: the case, the MicroVM whose endpoint the client sends
/// to, the guest path it addresses, the method, the token it presents (where it presents
/// one) and the status the endpoint must answer with.
type EndpointCase<'a> = (&'a str, &'a str, &'a str, &'a str, Option<&'a str>, u16);

/// One request a client sends through a MicroVM's published endpoint: the guest's own
/// path, which is the whole path of a request to that address (`<scheme>://<endpoint>` +
/// the guest path, as AWS documents), and the `X-aws-proxy-auth` token where the client
/// presents one.
fn guest_request(method: &str, path: &str, token: Option<&str>) -> http::Request<Vec<u8>> {
    let path = if path.is_empty() { "/" } else { path };
    let mut builder = http::Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(AUTH_TOKEN_HEADER, token);
    }
    builder
        .body(Vec::new())
        .expect("a well-formed request to a MicroVM endpoint")
}

/// Send `request` to the endpoint of `microvm_id` — the address that MicroVM alone is
/// published on — and report the status, the content type and the body as text.
fn through_endpoint(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    microvm_id: &str,
    request: &http::Request<Vec<u8>>,
) -> impl std::future::Future<Output = (u16, String, String)> {
    let state = engine.state::<MicrovmState>();
    let connectors = connector_state(&**engine);
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let request = request.clone();
    let microvm_id = microvm_id.to_string();
    async move {
        let response =
            serve_request(&state, connectors.as_ref(), &callback, &microvm_id, &request).await;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = String::from_utf8_lossy(response.body()).to_string();
        (status, content_type, body)
    }
}

/// How long a poll for one clock-owed convergence may take before the test gives up:
/// generous, because what is asserted is that the transition HAPPENS, not when.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long one poll of a settling resource waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Poll `GetMicrovmImage` until the image `name` has settled with an active version, so
/// what follows waits for the CONDITION rather than for a fixed time.
async fn settled_image(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
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
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the image {name} never settled with an active version: last read {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `GetMicrovm` until `microvm_id` reports `state`.
async fn microvm_in_state(router: &MultiServiceRouter, microvm_id: &str, state: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = call(
            router,
            "GET",
            &format!("/2025-09-09/microvms/{microvm_id}"),
            None,
        )
        .await;
        assert_eq!(status, 200, "read the MicroVM {microvm_id}: got {status} {read}");
        if read["state"].as_str() == Some(state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the MicroVM {microvm_id} never reached {state}: last read {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run a MicroVM from `image` and return `(microvmId, endpoint)` once it is RUNNING.
async fn running_microvm(router: &MultiServiceRouter, image: &str) -> (String, String) {
    let (status, run) = run_microvm(router, json!({"imageIdentifier": image})).await;
    assert_eq!(status, 200, "run a MicroVM from {image}: got {status} {run}");
    let microvm_id = run["microvmId"].as_str().unwrap_or_default().to_string();
    microvm_in_state(router, &microvm_id, "RUNNING").await;
    (
        microvm_id,
        run["endpoint"].as_str().unwrap_or_default().to_string(),
    )
}

/// Mint an authentication token for `microvm_id` over the wire and return its value.
async fn auth_token(
    router: &MultiServiceRouter,
    microvm_id: &str,
    expiration_in_minutes: u32,
    allowed_ports: Value,
) -> String {
    let (status, response) = call(
        router,
        "POST",
        &format!("/2025-09-09/microvms/{microvm_id}/auth-token"),
        Some(json!({
            "expirationInMinutes": expiration_in_minutes,
            "allowedPorts": allowed_ports,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "mint a token for {microvm_id}: got {status} {response}"
    );
    response["authToken"][AUTH_TOKEN_HEADER]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The `host:port` a published endpoint is, as a socket address — so a test can prove
/// the published value is an AUTHORITY a client on this machine can open, carrying no
/// scheme and no path.
fn endpoint_socket_addr(endpoint: &str) -> std::net::SocketAddr {
    endpoint
        .parse()
        .unwrap_or_else(|error| panic!("endpoint {endpoint:?} has no socket address: {error}"))
}

/// RULE: a published `endpoint` is the AUTHORITY of an address THE EMULATOR ANSWERS ON —
/// a host, optionally `host:port`, with no scheme and no path, because a client composes
/// its request URL by prefixing the scheme and appending the guest's own path. Each
/// MicroVM is published on an address of its own, a client can open it, and the reader
/// (`GetMicrovm`) reports the same endpoint the launch did.
#[tokio::test(flavor = "multi_thread")]
async fn a_published_microvm_endpoint_is_an_address_a_client_can_open() {
    let (router, _engine) = emulator_with_engine();
    built_image(&router, "endpoint-bearing").await;

    // Rows differ in what the launch asks for, because none of it changes the FORM of
    // the published address: the request members, the hook port its image declares, and
    // how many MicroVMs the store already holds.
    let rows: Vec<(&str, Value)> = vec![
        ("required members only", json!({"imageIdentifier": "endpoint-bearing"})),
        (
            "a second MicroVM of the same image",
            json!({"imageIdentifier": "endpoint-bearing"}),
        ),
        (
            "a bounded lifetime and an idle policy",
            json!({
                "imageIdentifier": "endpoint-bearing",
                "maximumDurationInSeconds": 900,
                "idlePolicy": {
                    "maxIdleDurationSeconds": 600,
                    "suspendedDurationSeconds": 300,
                    "autoResumeEnabled": true,
                },
            }),
        ),
        (
            "network connectors on both sides",
            json!({
                "imageIdentifier": "endpoint-bearing",
                "ingressNetworkConnectors": ["HTTP_INGRESS"],
                "egressNetworkConnectors": ["INTERNET_EGRESS"],
            }),
        ),
    ];

    let mut published: Vec<(String, String)> = Vec::new();
    for (case, body) in rows {
        let (status, run) = run_microvm(&router, body).await;
        assert_eq!(status, 200, "case {case:?}: got {status} {run}");
        let microvm_id = run["microvmId"].as_str().unwrap_or_default().to_string();
        let endpoint = run["endpoint"].as_str().unwrap_or_default().to_string();

        // The published value is an authority and nothing else: the shape a client
        // prefixes a scheme onto and appends the guest's path to.
        assert!(
            endpoint
                .chars()
                .all(|byte| byte.is_ascii_alphanumeric() || ".-:".contains(byte)),
            "case {case:?}: expected a bare host, optionally with a port, got {endpoint:?}"
        );
        assert!(
            !endpoint.contains("://") && !endpoint.contains('/'),
            "case {case:?}: expected no scheme and no path, got {endpoint:?}"
        );
        assert!(
            !endpoint.contains(&microvm_id),
            "case {case:?}: the MicroVM's id belongs in the address, not in the \
             published value, got {endpoint:?}"
        );

        // The address is this emulator's own: a loopback host and a port a client on
        // this machine can connect to. Opening the connection is the whole point —
        // before the fix the announced host resolved off this machine, so a client got
        // a transport failure and no HTTP status at all. Only the emulator's own
        // loopback port is ever dialled, and the row above has already refused any
        // address that is not loopback.
        let addr = endpoint_socket_addr(&endpoint);
        assert!(
            addr.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                || addr.ip() == std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            "case {case:?}: expected the emulator's own loopback address, got {addr}"
        );
        assert_ne!(addr.port(), 0, "case {case:?}: expected a bound port");
        let dialled = match addr.ip() {
            std::net::IpAddr::V4(_) => std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, addr.port())),
            std::net::IpAddr::V6(_) => std::net::TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, addr.port())),
        };
        if let Err(error) = dialled {
            panic!("case {case:?}: expected {addr} to accept a connection, got {error}");
        }

        // The reader reports the stored record, so it publishes the same address.
        let (status, read) = call(
            &router,
            "GET",
            &format!("/2025-09-09/microvms/{microvm_id}"),
            None,
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: read back: got {status} {read}");
        assert_eq!(
            read["endpoint"].as_str(),
            Some(endpoint.as_str()),
            "case {case:?}: GetMicrovm must report the published endpoint, got {read}"
        );

        published.push((microvm_id, endpoint));
    }

    // Each MicroVM is published on an address of its OWN, which is what carries the
    // discriminator the published value no longer does.
    let mut addresses: Vec<&str> = published
        .iter()
        .map(|(_, endpoint)| endpoint.as_str())
        .collect();
    addresses.sort_unstable();
    let count = addresses.len();
    addresses.dedup();
    assert_eq!(
        addresses.len(),
        count,
        "expected a distinct endpoint per MicroVM, got {addresses:?}"
    );
}

/// RULE: a published endpoint answers an HTTP STATUS for every request — `404` where
/// the address names no MicroVM that can be connected to, `403` where no live token
/// `CreateMicrovmAuthToken` minted for THAT MicroVM (and covering the port the request
/// addresses) is presented in `X-aws-proxy-auth`, and otherwise the GUEST's own answer —
/// which, for a MicroVM whose image holds no guest program this emulator's engine can run
/// (these images' code artifacts were never served), is the status the proxy answers for a
/// MicroVM it reached nothing inside of. Never a transport failure, whatever the guest
/// path, the method or the token.
#[tokio::test(flavor = "multi_thread")]
async fn a_published_endpoint_answers_an_http_status_for_every_request_class() {
    let (router, engine) = emulator_with_engine();

    // One image DECLARES a hooks port, one declares none: nothing of the image takes
    // part in the token decision, so both are judged against the port the request
    // addresses.
    let (status, created) = call(
        &router,
        "POST",
        "/2025-09-09/microvm-images",
        Some(image_body(
            "ported",
            json!({"hooks": {"port": 8080}}),
        )),
    )
    .await;
    assert_eq!(status, 201, "create the ported image: got {status} {created}");
    built_image(&router, "portless").await;
    // Both images' builds are clock-owed: wait for the CONDITION each test row needs —
    // an active version to launch from — rather than for a fixed time.
    settled_image(&router, "ported").await;
    settled_image(&router, "portless").await;

    let (ported_id, _ported) = running_microvm(&router, "ported").await;
    let (portless_id, _portless) = running_microvm(&router, "portless").await;
    let (other_id, _other) = running_microvm(&router, "ported").await;

    let every_port = auth_token(&router, &ported_id, 15, json!([{"allPorts": {}}])).await;
    let exact_port = auth_token(&router, &ported_id, 15, json!([{"port": 8080}])).await;
    let covering_range = auth_token(
        &router,
        &ported_id,
        15,
        json!([{"range": {"startPort": 8000, "endPort": 9000}}]),
    )
    .await;
    let other_port = auth_token(&router, &ported_id, 15, json!([{"port": 9090}])).await;
    let other_range = auth_token(
        &router,
        &ported_id,
        15,
        json!([{"range": {"startPort": 9000, "endPort": 9100}}]),
    )
    .await;
    let portless_every_port = auth_token(&router, &portless_id, 1, json!([{"allPorts": {}}])).await;
    let portless_one_port = auth_token(&router, &portless_id, 1, json!([{"port": 443}])).await;
    let other_microvm_token = auth_token(&router, &other_id, 15, json!([{"allPorts": {}}])).await;

    // An id no MicroVM of this store carries: an address that publishes nothing.
    let absent_id = "mvm-ffffffffffffffff";

    // (case, the MicroVM published at the address, the guest path, the method, the
    // token, status)
    let rows: Vec<EndpointCase> = vec![
        ("no token at all", &ported_id, "/health", "GET", None, 403),
        (
            "a token value nothing minted",
            &ported_id,
            "/health",
            "GET",
            Some("not-a-token-this-emulator-minted"),
            403,
        ),
        (
            "a live token minted for another MicroVM",
            &ported_id,
            "/health",
            "GET",
            Some(&other_microvm_token),
            403,
        ),
        (
            "a token whose allowedPorts name another port",
            &ported_id,
            "/health",
            "GET",
            Some(&other_port),
            403,
        ),
        (
            "a token whose range excludes the addressed port",
            &ported_id,
            "/health",
            "GET",
            Some(&other_range),
            403,
        ),
        (
            "a token for every port",
            &ported_id,
            "/health",
            "GET",
            Some(&every_port),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "a token naming the addressed port exactly",
            &ported_id,
            "/health",
            "GET",
            Some(&exact_port),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "a token whose range covers the addressed port",
            &ported_id,
            "/health",
            "GET",
            Some(&covering_range),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "a MicroVM whose image publishes no port, with a token for every port",
            &portless_id,
            "/health",
            "GET",
            Some(&portless_every_port),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "a MicroVM whose image publishes no port, with a token scoped to a port the \
             request does not address",
            &portless_id,
            "/health",
            "GET",
            Some(&portless_one_port),
            403,
        ),
        (
            "a guest path carrying a query",
            &ported_id,
            "/connect?host=10.90.1.17&port=443",
            "GET",
            Some(&every_port),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "the guest's root path",
            &ported_id,
            "/",
            "GET",
            Some(&every_port),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "no path past the endpoint at all",
            &ported_id,
            "",
            "GET",
            Some(&every_port),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "a deep guest path",
            &ported_id,
            "/microvms/mvm-0000000000000000/health",
            "GET",
            Some(&every_port),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "a POST to the guest",
            &ported_id,
            "/health",
            "POST",
            Some(&every_port),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "a HEAD of the guest",
            &ported_id,
            "/health",
            "HEAD",
            Some(&every_port),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "an address publishing a MicroVM this store never minted",
            absent_id,
            "/health",
            "GET",
            Some(&every_port),
            404,
        ),
        (
            "that same address with no token at all",
            absent_id,
            "/",
            "GET",
            None,
            404,
        ),
    ];

    for (case, microvm_id, path, method, token, expected) in rows {
        let request = guest_request(method, path, token);
        let (status, content_type, body) =
            through_endpoint(&router, &engine, microvm_id, &request).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected {expected}, got {status} {body:?}"
        );
        assert_eq!(
            content_type, "text/plain; charset=utf-8",
            "case {case:?}: expected a plain-text answer, got {content_type:?}"
        );
        if method == "HEAD" {
            assert!(
                body.is_empty(),
                "case {case:?}: a HEAD carries no body, got {body:?}"
            );
        } else {
            assert!(
                !body.is_empty(),
                "case {case:?}: expected the endpoint's own wording, got an empty body"
            );
        }
    }

    // A request carrying NO authentication is refused for THAT, which is the one thing
    // the proxy's answer names.
    let unauthenticated = guest_request("GET", "/health", None);
    let (status, _, body) =
        through_endpoint(&router, &engine, &ported_id, &unauthenticated).await;
    assert_eq!(status, 403, "an unauthenticated request is refused, got {status}");
    assert!(
        body.contains("missing authentication"),
        "expected the refusal to name the missing authentication, got {body:?}"
    );
}

/// RULE: a token is accepted only while it is LIVE — the clock's own expiry, and the
/// MicroVM's: once `expirationInMinutes` has run out, or the MicroVM can no longer be
/// connected to, the same request that was served is refused with `403`, and the
/// address of a MicroVM on its way out answers `404`.
#[tokio::test(flavor = "multi_thread")]
async fn a_token_stops_being_accepted_once_the_clock_or_the_microvm_has_ended_it() {
    let (router, engine) = emulator_with_engine();
    built_image(&router, "expiring").await;
    let (microvm_id, _endpoint) = running_microvm(&router, "expiring").await;
    let (doomed_id, _doomed) = running_microvm(&router, "expiring").await;

    let token = auth_token(&router, &microvm_id, 15, json!([{"allPorts": {}}])).await;
    let doomed_token = auth_token(&router, &doomed_id, 15, json!([{"allPorts": {}}])).await;

    // The control: both tokens are live, so both endpoints answer past the token check.
    for (case, microvm_id, token) in [
        ("the MicroVM whose token is about to expire", &microvm_id, &token),
        ("the MicroVM about to be terminated", &doomed_id, &doomed_token),
    ] {
        let request = guest_request("GET", "/health", Some(token));
        let (status, _, body) = through_endpoint(&router, &engine, microvm_id, &request).await;
        assert_eq!(
            status,
            NO_GUEST_RESPONSE_STATUS,
            "case {case:?}: a live token must pass the token check, got {status} {body:?}"
        );
    }

    // The clock expires the token: the store holds its digest with the instant it stops
    // being accepted, so moving that instant into the past is the same thing as waiting
    // out `expirationInMinutes`.
    {
        let state = engine.state::<MicrovmState>();
        let mut store = state.lock();
        let digest = auth_token_digest(&token);
        let record = store
            .auth_tokens
            .get_mut(&digest)
            .expect("the minted token is recorded against the MicroVM");
        record.expires_at = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
    }
    let request = guest_request("GET", "/health", Some(&token));
    let (status, _, body) = through_endpoint(&router, &engine, &microvm_id, &request).await;
    assert_eq!(
        status, 403,
        "an expired token must be refused, got {status} {body:?}"
    );

    // And a MicroVM that can no longer be connected to is published nowhere: its
    // address answers 404 from the moment it starts terminating, token or no token.
    let (status, terminated) = call(
        &router,
        "DELETE",
        &format!("/2025-09-09/microvms/{doomed_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "terminate the MicroVM: got {status} {terminated}"
    );
    for (case, token) in [
        ("with the token minted for it", Some(doomed_token.as_str())),
        ("with no token at all", None),
    ] {
        let request = guest_request("GET", "/health", token);
        let (status, _, body) = through_endpoint(&router, &engine, &doomed_id, &request).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a MicroVM on its way out is published nowhere, got {status} {body:?}"
        );
    }
    // The termination is clock-owed, so this waits for the MicroVM to REACH its terminal
    // state rather than for a fixed time.
    microvm_in_state(&router, &doomed_id, "TERMINATED").await;
    let request = guest_request("GET", "/health", Some(&doomed_token));
    let (status, _, body) = through_endpoint(&router, &engine, &doomed_id, &request).await;
    assert_eq!(
        status, 404,
        "a terminated MicroVM is published nowhere, got {status} {body:?}"
    );
}

/// RULE: inbound traffic through a MicroVM's endpoint is what its `idlePolicy` measures —
/// "idle time is measured by inbound traffic through the MicroVM proxy endpoint" — so a
/// request a token GRANTED puts the automatic suspend a whole `maxIdleDurationSeconds`
/// away again, a request the endpoint REFUSED reached the MicroVM with nothing and moves
/// nothing, and a request that arrives while the MicroVM rests brings it back exactly
/// where `autoResumeEnabled` says so.
#[tokio::test(flavor = "multi_thread")]
async fn inbound_traffic_through_the_endpoint_is_what_an_idle_policy_measures() {
    let (router, engine) = emulator_with_engine();
    built_image(&router, "idle-policy").await;

    // The idle duration every row carries, well above the figure a refreshed deadline is
    // asserted against, so the assertions are about WHICH deadline is owed rather than
    // about how fast the host runs.
    let max_idle = Duration::from_secs(600);

    // The rows differ along `autoResumeEnabled`, the one member that decides what a
    // request to a resting MicroVM does.
    for (case, auto_resume, after_traffic) in [
        ("a policy that auto-resumes", true, "RUNNING"),
        ("a policy that does not", false, "SUSPENDED"),
    ] {
        let (status, run) = run_microvm(
            &router,
            json!({
                "imageIdentifier": "idle-policy",
                "idlePolicy": {
                    "maxIdleDurationSeconds": max_idle.as_secs(),
                    "suspendedDurationSeconds": 300,
                    "autoResumeEnabled": auto_resume,
                },
            }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: run got {status} {run}");
        let microvm_id = run["microvmId"].as_str().unwrap_or_default().to_string();
        microvm_in_state(&router, &microvm_id, "RUNNING").await;
        let token = auth_token(&router, &microvm_id, 15, json!([{"allPorts": {}}])).await;

        // A deadline a few seconds away, so what the request does to it is unmistakable.
        let nearly_due = Duration::from_secs(5);
        set_deadline(&engine, &microvm_id, Deadline::Idle, Some(nearly_due));
        let granted = guest_request("GET", "/health", Some(&token));
        let (status, _, body) = through_endpoint(&router, &engine, &microvm_id, &granted).await;
        assert_eq!(
            status,
            NO_GUEST_RESPONSE_STATUS,
            "case {case:?}: the token must grant the request, got {status} {body:?}"
        );
        let left = deadline_left(&engine, &microvm_id, Deadline::Idle)
            .unwrap_or_else(|| panic!("case {case:?}: a MicroVM with a policy owes a suspend"));
        assert!(
            left > nearly_due && left <= max_idle,
            "case {case:?}: traffic must put the suspend a whole idle duration away, got \
             {left:?}"
        );

        // A refused request is no traffic at all: it leaves the deadline where it was.
        set_deadline(&engine, &microvm_id, Deadline::Idle, Some(nearly_due));
        let refused = guest_request("GET", "/health", Some("not-a-token-this-emulator-minted"));
        let (status, _, body) = through_endpoint(&router, &engine, &microvm_id, &refused).await;
        assert_eq!(
            status, 403,
            "case {case:?}: the request must be refused, got {status} {body:?}"
        );
        let left = deadline_left(&engine, &microvm_id, Deadline::Idle)
            .unwrap_or_else(|| panic!("case {case:?}: the suspend is still owed"));
        assert!(
            left <= nearly_due,
            "case {case:?}: a refused request moves no deadline, got {left:?}"
        );

        // The platform suspends it once the idle duration has passed with no traffic, and
        // the next request then decides what the policy says it decides.
        set_deadline(&engine, &microvm_id, Deadline::Idle, None);
        microvm_in_state(&router, &microvm_id, "SUSPENDED").await;
        let granted = guest_request("GET", "/health", Some(&token));
        let (status, _, body) = through_endpoint(&router, &engine, &microvm_id, &granted).await;
        assert_eq!(
            status,
            NO_GUEST_RESPONSE_STATUS,
            "case {case:?}: the address still answers a granted request, got {status} {body:?}"
        );
        let (status, read) = call(
            &router,
            "GET",
            &format!("/2025-09-09/microvms/{microvm_id}"),
            None,
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: read got {status} {read}");
        assert_eq!(
            read["state"].as_str(),
            Some(after_traffic),
            "case {case:?}: expected {after_traffic} after a request reached the resting \
             MicroVM, got {read}"
        );
    }
}

/// RULE: `expirationInMinutes` is modelled with no maximum, so the service's own ceiling
/// decides: a request for a token longer-lived than the maximum is refused with the
/// modelled 400 `ValidationException` naming the value it asked for and the limit, the
/// boundary value itself is minted, and the refusal is taken AFTER the MicroVM lookup, so
/// an identifier naming no MicroVM is still answered with the 404.
#[tokio::test(flavor = "multi_thread")]
async fn a_token_is_minted_only_within_the_services_expiration_maximum() {
    let (router, _engine) = emulator_with_engine();
    built_image(&router, "token-bearing").await;
    // The shell connector as well as the HTTP one, so the same table drives BOTH mints of
    // this API: `CreateMicrovmShellAuthToken` is served only for a MicroVM run with
    // `SHELL_INGRESS` attached.
    let (status, run) = run_microvm(
        &router,
        json!({
            "imageIdentifier": "token-bearing",
            "ingressNetworkConnectors": ["HTTP_INGRESS", "SHELL_INGRESS"],
        }),
    )
    .await;
    assert_eq!(status, 200, "run a MicroVM: got {status} {run}");
    let microvm_id = run["microvmId"].as_str().unwrap_or_default().to_string();
    microvm_in_state(&router, &microvm_id, "RUNNING").await;

    let cap = MAX_AUTH_TOKEN_EXPIRATION_MINUTES;
    // Rows span the member's own range around the service maximum: its smallest value,
    // one inside the cap, the boundary itself, one past it, and values far past it.
    let rows: Vec<(&str, i32, bool)> = vec![
        ("the smallest lifetime the member admits", 1, true),
        ("a lifetime inside the maximum", cap - 1, true),
        ("the maximum itself", cap, true),
        ("one minute past the maximum", cap + 1, false),
        ("a whole day", 24 * 60, false),
        ("the largest value the member admits", i32::MAX, false),
    ];

    for (case, expiration_in_minutes, minted) in rows {
        for mint in ["auth-token", "shell-auth-token"] {
            let (status, response) = call(
                &router,
                "POST",
                &format!("/2025-09-09/microvms/{microvm_id}/{mint}"),
                Some(json!({
                    "expirationInMinutes": expiration_in_minutes,
                    "allowedPorts": [{"allPorts": {}}],
                })),
            )
            .await;
            if minted {
                assert_eq!(
                    status, 200,
                    "case {case:?} on {mint}: expected a minted token, got {status} {response}"
                );
                assert!(
                    response["authToken"][AUTH_TOKEN_HEADER].is_string(),
                    "case {case:?} on {mint}: expected a token, got {response}"
                );
            } else {
                assert_eq!(
                    status, 400,
                    "case {case:?} on {mint}: expected the service maximum to refuse it, \
                     got {status} {response}"
                );
                assert_eq!(
                    response["__type"].as_str(),
                    Some("ValidationException"),
                    "case {case:?} on {mint}: expected ValidationException, got {response}"
                );
                assert_eq!(
                    response["message"].as_str(),
                    Some(
                        auth_token_expiration_exceeded_message(expiration_in_minutes).as_str()
                    ),
                    "case {case:?} on {mint}: expected the value asked for and the limit, \
                     got {response}"
                );
            }
        }
    }

    // The lookup comes first: an identifier naming no MicroVM is the 404 even when the
    // lifetime asked for is one the maximum refuses.
    let absent = "microvm-00000000-0000-0000-0000-000000000000";
    for mint in ["auth-token", "shell-auth-token"] {
        let (status, response) = call(
            &router,
            "POST",
            &format!("/2025-09-09/microvms/{absent}/{mint}"),
            Some(json!({
                "expirationInMinutes": cap + 1,
                "allowedPorts": [{"allPorts": {}}],
            })),
        )
        .await;
        assert_eq!(
            status, 404,
            "on {mint}: the MicroVM lookup is decided first, got {status} {response}"
        );
        assert_eq!(
            response["message"].as_str(),
            Some(microvm_not_found_message(absent).as_str()),
            "on {mint}: expected the not-found built from the identifier, got {response}"
        );
    }
}
