// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the MicroVM data plane's two decisions a client depends on: WHICH
//! requests a minted authentication token grants, and WHAT the guest behind the endpoint
//! is — one process, with its own memory, its own disk and its own identity, across every
//! request and across a suspend.
//!
//! Each test states a RULE and exercises it over a SET of inputs; the ports, keys and ids
//! any one row carries are examples, not the specification. The control plane is driven
//! over the wire through the router the emulator boots, with Amazon S3 mounted beside this
//! service so the guest's code artifact is staged the way a client stages one; the
//! endpoint itself is driven through `state::microvm_endpoint::serve_request`, the one
//! decision the core's HTTPS listener hands every request on that address to.
//!
//! The guest's program is written here and packaged into that artifact: it keeps a value
//! in memory and a value on disk, reports the identity its machine published to it, and
//! answers nothing this emulator chose.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_endpoint::{
    serve_request, AUTH_TOKEN_HEADER, NO_GUEST_RESPONSE_STATUS, PROXY_PORT_HEADER,
};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_network::connector_state;
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::ComAmazonawsLambdamicrovmsLambdamicrovmsState as MicrovmState;
use smithy_server_runtime::MultiServiceRouter;

use crate::microvm_guest_data_plane::{emulator, guest_artifact, stage_artifact};
use crate::microvm_lifecycle::{call, image_body};

/// How long the whole poll for one clock-owed convergence may take before a test gives
/// up: generous, because what is asserted is that the transition HAPPENS, not when.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long one poll of a settling resource waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The variable the image declares for its guest, and the value it carries — the guest
/// reports it back, so the test reads the environment the image published rather than one
/// this emulator chose.
pub(crate) const MARKER_VARIABLE: &str = "SUITE_MARKER";

/// The port this suite's guest binds its server on, which is therefore the port its
/// MicroVM's endpoint fronts.
const GUEST_PORT: i32 = 8080;

/// The guest this suite runs: a Node-shaped HTTP server that keeps an id minted when its
/// own process started, a counter and a value in memory, the same value on disk, and
/// reports the identity its machine published to it.
const GUEST_SERVER: &str = r#"
const http = require('http');
const url = require('url');
const fs = require('fs');
const os = require('os');

/* Minted by the GUEST, once, when its process starts: two reads that report the same
   value are two reads of the same process. */
const processBootId = Math.random().toString(36).slice(2) + Math.random().toString(36).slice(2);
let counter = 0;
const inMemory = {};

function identity() {
    const stat = fs.readFileSync('/proc/self/stat', 'utf8').split(' ');
    return {
        processBootId: processBootId,
        machineBootId: fs.readFileSync('/proc/sys/kernel/random/boot_id', 'utf8').trim(),
        pid: process.pid,
        starttime: Number(stat[21]),
        hostname: os.hostname(),
        marker: process.env.SUITE_MARKER === undefined ? null : process.env.SUITE_MARKER
    };
}

http.createServer(function (req, res) {
    const parsed = url.parse(req.url, true);
    const answer = function (status, body) {
        res.writeHead(status, {'content-type': 'application/json'});
        res.end(JSON.stringify(body));
    };
    if (parsed.pathname === '/id') { answer(200, identity()); return; }
    if (parsed.pathname === '/count') { counter = counter + 1; answer(200, {count: counter}); return; }
    if (parsed.pathname === '/set') {
        const key = parsed.query.key;
        const value = parsed.query.value;
        inMemory[key] = value;
        fs.writeFileSync('/data/' + key, value);
        answer(200, {stored: key});
        return;
    }
    if (parsed.pathname === '/get') {
        const key = parsed.query.key;
        const path = '/data/' + key;
        answer(200, {
            memory: inMemory[key] === undefined ? null : inMemory[key],
            disk: fs.existsSync(path) ? fs.readFileSync(path, 'utf8') : null
        });
        return;
    }
    answer(404, {error: 'no such path'});
}).listen(8080);
"#;

/// Create the image whose code artifact is the staged guest, with `env` as its
/// `environmentVariables`, and poll it until its build has settled with an active
/// version.
pub(crate) async fn guest_image(
    router: &MultiServiceRouter,
    name: &str,
    uri: &str,
    env: Value,
) -> String {
    let (status, created) = call(
        router,
        "POST",
        "/2025-09-09/microvm-images",
        Some(image_body(
            name,
            json!({"codeArtifact": {"uri": uri}, "environmentVariables": env}),
        )),
    )
    .await;
    assert_eq!(status, 201, "create the image {name}: got {status} {created}");
    let image_arn = created["imageArn"]
        .as_str()
        .expect("the image carries an ARN")
        .to_string();

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
            return image_arn;
        }
        assert!(
            Instant::now() < deadline,
            "the image {name} never settled with an active version: last read {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `GetMicrovm` until `microvm_id` reports `state`, which is how a client waits out
/// a clock-owed transition.
pub(crate) async fn microvm_in_state(router: &MultiServiceRouter, microvm_id: &str, state: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = call(
            router,
            "GET",
            &format!("/2025-09-09/microvms/{microvm_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "read the MicroVM {microvm_id}: got {status} {read}"
        );
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

/// Run a MicroVM from `image`, poll it until it reports `RUNNING`, and answer its id and
/// published endpoint.
pub(crate) async fn running_microvm(router: &MultiServiceRouter, image: &str) -> (String, String) {
    let (status, run) = call(
        router,
        "POST",
        "/2025-09-09/microvms",
        Some(json!({"imageIdentifier": image})),
    )
    .await;
    assert_eq!(status, 200, "run a MicroVM from {image}: got {status} {run}");
    let microvm_id = run["microvmId"]
        .as_str()
        .expect("the MicroVM carries an id")
        .to_string();
    let endpoint = run["endpoint"]
        .as_str()
        .expect("the MicroVM carries an endpoint")
        .to_string();
    microvm_in_state(router, &microvm_id, "RUNNING").await;
    (microvm_id, endpoint)
}

/// Mint an authentication token for `microvm_id` scoped to `allowed_ports`.
pub(crate) async fn auth_token(
    router: &MultiServiceRouter,
    microvm_id: &str,
    allowed_ports: Value,
) -> String {
    let (status, minted) = call(
        router,
        "POST",
        &format!("/2025-09-09/microvms/{microvm_id}/auth-token"),
        Some(json!({"expirationInMinutes": 15, "allowedPorts": allowed_ports})),
    )
    .await;
    assert_eq!(
        status, 200,
        "mint a token for {microvm_id}: got {status} {minted}"
    );
    minted["authToken"][AUTH_TOKEN_HEADER]
        .as_str()
        .expect("the token carries the proxy header's value")
        .to_string()
}

/// One request through a MicroVM's published endpoint: the status and the body it
/// answered, with `port` named in `X-aws-proxy-port` where the client names one.
///
/// `microvm_id` is the MicroVM whose own address the request is sent to — the published
/// endpoint is that address's authority, so the request's whole path is the guest's.
pub(crate) async fn through_endpoint(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    microvm_id: &str,
    path: &str,
    token: &str,
    port: Option<&str>,
) -> (u16, Value) {
    let mut builder = http::Request::builder()
        .method("GET")
        .uri(path)
        .header(AUTH_TOKEN_HEADER, token);
    if let Some(port) = port {
        builder = builder.header(PROXY_PORT_HEADER, port);
    }
    let request = builder
        .body(Vec::new())
        .expect("a well-formed request to a MicroVM endpoint");
    let state = engine.state::<MicrovmState>();
    let connectors = connector_state(&**engine);
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let response =
        serve_request(&state, connectors.as_ref(), &callback, microvm_id, &request).await;
    let status = response.status().as_u16();
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, body)
}

/// The guest's answer to `path`, which must be the guest's own 200.
pub(crate) async fn guest_answer(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    microvm_id: &str,
    path: &str,
    token: &str,
) -> Value {
    let (status, body) = through_endpoint(router, engine, microvm_id, path, token, None).await;
    assert_eq!(
        status, 200,
        "GET {path} on the endpoint of {microvm_id}: expected the guest's own answer, \
         got {status} {body}"
    );
    body
}

/// Everything these suites stand up: the emulator, the staged guest and the image that
/// runs it.
async fn standing() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>, String) {
    let (router, engine) = emulator();
    stage_artifact(
        &router,
        "mvm-process-artifacts",
        "guest.zip",
        guest_artifact(GUEST_SERVER),
    )
    .await;
    let image = guest_image(
        &router,
        "guest-process",
        "s3://mvm-process-artifacts/guest.zip",
        json!({MARKER_VARIABLE: "a-published-value"}),
    )
    .await;
    (router, engine, image)
}

/// RULE: a MicroVM runs its guest as ONE process — what that process holds in memory,
/// what it wrote to its machine's disk and the identity its machine published to it are
/// the same at every request, and a `SuspendMicrovm` + `ResumeMicrovm` preserves all of
/// it ("preserving its full memory and disk state", restored "with all state intact")
/// while the suspended MicroVM itself answers no request. Another MicroVM of the same
/// image is another machine running another process, with none of the first's memory,
/// disk or identity.
#[tokio::test(flavor = "multi_thread")]
async fn a_microvms_guest_is_one_process_with_its_own_memory_disk_and_identity() {
    let (router, engine, image) = standing().await;
    let (first_id, _first_endpoint) = running_microvm(&router, &image).await;
    let (second_id, _second_endpoint) = running_microvm(&router, &image).await;
    let token = auth_token(&router, &first_id, json!([{"allPorts": {}}])).await;
    let other_token = auth_token(&router, &second_id, json!([{"allPorts": {}}])).await;

    // The identity the MicroVM published to its guest, read twice: a process answers the
    // same identity however often it is asked.
    let identity = guest_answer(&router, &engine, &first_id, "/id", &token).await;
    let again = guest_answer(&router, &engine, &first_id, "/id", &token).await;
    assert_eq!(
        identity, again,
        "two reads of one running MicroVM must report one identity, got {identity} then {again}"
    );
    assert_eq!(
        identity["hostname"].as_str(),
        Some(first_id.as_str()),
        "the machine's host name is the MicroVM's own id, got {identity}"
    );
    assert_eq!(
        identity["marker"].as_str(),
        Some("a-published-value"),
        "the guest reads the environment its image declared, got {identity}"
    );
    let boot_id = identity["machineBootId"].as_str().unwrap_or_default();
    assert!(
        boot_id.len() == 36 && boot_id.split('-').count() == 5,
        "the kernel publishes this boot's id as a UUID, got {boot_id:?}"
    );
    assert!(
        identity["pid"].as_i64().unwrap_or_default() > 0,
        "the guest's process has a pid of its own, got {identity}"
    );
    assert!(
        identity["starttime"].as_i64().unwrap_or_default() >= 0,
        "the guest's process reports when it started, got {identity}"
    );

    // The memory and the disk of that one process: each request reaches what the request
    // before it left behind.
    for expected in 1..=3 {
        let counted = guest_answer(&router, &engine, &first_id, "/count", &token).await;
        assert_eq!(
            counted["count"].as_i64(),
            Some(expected),
            "request {expected} must reach the same process's counter, got {counted}"
        );
    }
    // (case, the key, the value as the request carries it, the value the guest keeps)
    let kept: Vec<(&str, &str, &str, &str)> = vec![
        ("a plain value", "alpha", "first-value", "first-value"),
        (
            "a value with room in it, as the query encodes one",
            "beta",
            "second%20value",
            "second value",
        ),
        ("a replacement of the first key", "alpha", "replaced", "replaced"),
    ];
    for (case, key, value, _) in &kept {
        let stored = guest_answer(
            &router,
            &engine,
            &first_id,
            &format!("/set?key={key}&value={value}"),
            &token,
        )
        .await;
        assert_eq!(
            stored["stored"].as_str(),
            Some(*key),
            "case {case:?}: the guest reports what it stored, got {stored}"
        );
    }
    // What the guest holds for each key now: the last value written for it.
    let held: Vec<(&str, &str)> = vec![("alpha", "replaced"), ("beta", "second value")];
    for (key, expected) in &held {
        let read = guest_answer(&router, &engine, &first_id, &format!("/get?key={key}"), &token).await;
        assert_eq!(
            read["memory"].as_str(),
            Some(*expected),
            "the value {key} kept IN MEMORY must survive the request that wrote it, got {read}"
        );
        assert_eq!(
            read["disk"].as_str(),
            Some(*expected),
            "the value {key} kept ON DISK must survive it too, got {read}"
        );
    }

    // A SUSPENDED MicroVM is not executing its guest, so the endpoint serves no 200 —
    // whichever of the two states the suspend is observed in.
    let (status, suspended) = call(
        &router,
        "POST",
        &format!("/2025-09-09/microvms/{first_id}/suspend"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 200, "suspend the MicroVM: got {status} {suspended}");
    microvm_in_state(&router, &first_id, "SUSPENDED").await;
    for path in ["/id", "/count", "/get?key=alpha"] {
        let (status, body) = through_endpoint(&router, &engine, &first_id, path, &token, None).await;
        assert_eq!(
            status, NO_GUEST_RESPONSE_STATUS,
            "GET {path} on a SUSPENDED MicroVM must not be answered by its guest, got \
             {status} {body}"
        );
    }

    // Resumed, it is the SAME process: the identity, the counter and both values are the
    // ones it held before the suspend.
    let (status, resumed) = call(
        &router,
        "POST",
        &format!("/2025-09-09/microvms/{first_id}/resume"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 200, "resume the MicroVM: got {status} {resumed}");
    microvm_in_state(&router, &first_id, "RUNNING").await;
    let after = guest_answer(&router, &engine, &first_id, "/id", &token).await;
    assert_eq!(
        after, identity,
        "a resumed MicroVM runs the process it suspended, so its identity is unchanged: \
         expected {identity}, got {after}"
    );
    let counted = guest_answer(&router, &engine, &first_id, "/count", &token).await;
    assert_eq!(
        counted["count"].as_i64(),
        Some(4),
        "the counter the process held through the suspend must carry on, got {counted}"
    );
    for (key, expected) in &held {
        let read = guest_answer(&router, &engine, &first_id, &format!("/get?key={key}"), &token).await;
        assert_eq!(
            (read["memory"].as_str(), read["disk"].as_str()),
            (Some(*expected), Some(*expected)),
            "the value {key} must survive the suspend in memory and on disk, got {read}"
        );
    }

    // The other MicroVM of the same image is another machine running another process.
    let other = guest_answer(&router, &engine, &second_id, "/id", &other_token).await;
    assert_eq!(
        other["hostname"].as_str(),
        Some(second_id.as_str()),
        "the second MicroVM's guest reports its own host name, got {other}"
    );
    assert_ne!(
        other["machineBootId"], identity["machineBootId"],
        "two MicroVMs are two boots, so their boot ids differ: got {other} and {identity}"
    );
    assert_ne!(
        other["processBootId"], identity["processBootId"],
        "two MicroVMs run two processes, so the id each minted differs: got {other} and \
         {identity}"
    );
    let counted = guest_answer(&router, &engine, &second_id, "/count", &other_token).await;
    assert_eq!(
        counted["count"].as_i64(),
        Some(1),
        "the second MicroVM's process holds none of the first's memory, got {counted}"
    );
    let read = guest_answer(&router, &engine, &second_id, "/get?key=alpha", &other_token).await;
    assert_eq!(
        (read["memory"].as_str(), read["disk"].as_str()),
        (None, None),
        "the second MicroVM's machine holds none of the first's disk, got {read}"
    );
}

/// One row of the port-scope table: the case, the `allowedPorts` the token is minted
/// with, the port the request names in `X-aws-proxy-port` (none where the client names
/// none), and the status the endpoint must answer with.
type PortScopeRow<'a> = (&'a str, Value, Option<&'a str>, u16);

/// RULE: a presented token grants a request when its `allowedPorts` cover the port THE
/// REQUEST ADDRESSES — the port named in `X-aws-proxy-port`, or the port the endpoint
/// fronts (the port the MicroVM's own guest bound) where the request names none — by the
/// `port`, `range` or `allPorts` arm of `PortSpecification`, and the request is refused
/// with `403` otherwise. Nothing of the image the MicroVM runs takes part in that
/// decision, and a granted request addressing a port the guest bound no server on reaches
/// nothing inside the MicroVM.
#[tokio::test(flavor = "multi_thread")]
async fn a_token_grants_the_port_the_request_addresses() {
    let (router, engine, image) = standing().await;
    let (microvm_id, _endpoint) = running_microvm(&router, &image).await;

    // The port this suite's guest bound its server on, which is therefore the port its
    // endpoint fronts, and one it bound nothing on.
    let fronted = GUEST_PORT.to_string();
    let elsewhere = "9999";

    // (case, allowedPorts, the port the request names, the status it must answer)
    let rows: Vec<PortScopeRow> = vec![
        (
            "the fronted port, named by neither side",
            json!([{"port": GUEST_PORT}]),
            None,
            200,
        ),
        (
            "the fronted port, named by the request too",
            json!([{"port": GUEST_PORT}]),
            Some(fronted.as_str()),
            200,
        ),
        (
            "a token for the fronted port, a request addressing another",
            json!([{"port": GUEST_PORT}]),
            Some(elsewhere),
            403,
        ),
        (
            "a token for another port, a request addressing the fronted one",
            json!([{"port": 9999}]),
            None,
            403,
        ),
        (
            "a range covering the addressed port",
            json!([{"range": {"startPort": 8000, "endPort": 9000}}]),
            None,
            200,
        ),
        (
            "a range whose lower bound IS the addressed port",
            json!([{"range": {"startPort": GUEST_PORT, "endPort": 9000}}]),
            None,
            200,
        ),
        (
            "a range whose upper bound IS the addressed port",
            json!([{"range": {"startPort": 1, "endPort": GUEST_PORT}}]),
            None,
            200,
        ),
        (
            "a range just above the addressed port",
            json!([{"range": {"startPort": 8081, "endPort": 9000}}]),
            None,
            403,
        ),
        (
            "a range just below the addressed port",
            json!([{"range": {"startPort": 1, "endPort": 8079}}]),
            None,
            403,
        ),
        (
            "every port, addressing the one the guest bound",
            json!([{"allPorts": {}}]),
            Some(fronted.as_str()),
            200,
        ),
        (
            "every port, addressing one the guest bound nothing on",
            json!([{"allPorts": {}}]),
            Some(elsewhere),
            NO_GUEST_RESPONSE_STATUS,
        ),
        (
            "several entries, one of which covers the addressed port",
            json!([{"port": 443}, {"range": {"startPort": 9000, "endPort": 9100}}, {"port": GUEST_PORT}]),
            None,
            200,
        ),
        (
            "several entries, none of which covers it",
            json!([{"port": 443}, {"range": {"startPort": 9000, "endPort": 9100}}]),
            None,
            403,
        ),
        (
            "a port the request names that is no port at all",
            json!([{"allPorts": {}}]),
            Some("not-a-port"),
            403,
        ),
        (
            "a port the request names outside the modelled range",
            json!([{"allPorts": {}}]),
            Some("70000"),
            403,
        ),
    ];

    for (case, allowed_ports, named, expected) in rows {
        let token = auth_token(&router, &microvm_id, allowed_ports.clone()).await;
        let (status, body) =
            through_endpoint(&router, &engine, &microvm_id, "/id", &token, named).await;
        assert_eq!(
            status, expected,
            "case {case:?}: a token for {allowed_ports} addressing {named:?} must be \
             answered {expected}, got {status} {body}"
        );
        if expected == 200 {
            assert!(
                body["machineBootId"].is_string(),
                "case {case:?}: a granted request is answered by the guest, got {body}"
            );
        }
    }
}
