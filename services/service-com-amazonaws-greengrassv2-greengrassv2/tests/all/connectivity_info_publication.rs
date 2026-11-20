// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the connectivity information a Greengrass core device publishes
//! for the client devices that connect to its MQTT broker.
//!
//! The rules under test:
//!
//! * `UpdateConnectivityInfo` REPLACES the endpoint list the named core device had
//!   published and answers the version token minted for the stored list — a fresh token
//!   per write, so a client can tell one write's list from another's;//! * `GetConnectivityInfo` reports exactly the endpoints the last write stored for that
//!   thing name, in the order they were written and with every member each entry carried,
//!   and reports a thing name nothing has been published for in the legacy surface's
//!   not-found envelope (HTTP 404 with the sentence in a `Message` body member naming that
//!   core device) rather than as a success carrying an empty list;
//! * each core device's record is its own: writing one thing name's endpoints changes no
//!   other thing name's; and
//! * a body this surface cannot read as connectivity information — a list with an entry
//!   that names no endpoint — is refused in the legacy envelope (HTTP 400 with the sentence
//!   in a `Message` body member) and nothing is stored.
//!
//! Every assertion drives the real `restJson1` front door: the request is built the way an
//! SDK builds it, dispatched through [`MultiServiceRouter`], and judged on the HTTP status
//! and the JSON body. Nothing reaches into the state struct, so an endpoint list that was
//! reported but never persisted fails here.
//!
//! The rows differ along what the rules generalise over — how many endpoints a write
//! carries, which optional members each entry carries, the form of the address (IPv4, IPv6
//! and DNS), the port (including each published bound of `PortNumberInt` and the one past
//! it), and the form of the core device thing name — not along names the rules say nothing
//! about.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_greengrassv2_greengrassv2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these requests are signed for.
pub(crate) const REGION: &str = "us-west-2";

/// The status and sentence this surface refuses a body it cannot read as connectivity
/// information with — written here as the literals AWS answers rather than read from the
/// implementation's constants, so a wording that drifts away from AWS fails this suite.
const UNREADABLE_STATUS: u16 = 400;
pub(crate) const UNREADABLE_BODY_MESSAGE: &str = "Invalid json format found in the request body.";

/// The status this surface reports a core device that has published no connectivity
/// information at, and the sentence that report carries — again the literals AWS answers.
/// The thing name is the one variable part of the sentence.
pub(crate) const NO_CONNECTIVITY_INFO_STATUS: u16 = 404;
pub(crate) fn no_connectivity_info_message(thing_name: &str) -> String {
    format!("No connectivity info found for thing {thing_name}.")
}

/// Assert that `status` and `body` are this surface's report of a core device that has
/// published no connectivity information: the status above, that one sentence naming
/// `thing_name`, and no endpoint list at all.
pub(crate) async fn assert_no_connectivity_info(
    router: &MultiServiceRouter,
    case: &str,
    thing_name: &str,
) {
    let (status, got) = read(router, thing_name).await;
    assert_eq!(
        (status, text(&got, "Message")),
        (
            NO_CONNECTIVITY_INFO_STATUS,
            no_connectivity_info_message(thing_name)
        ),
        "case {case:?}: expected the no-connectivity-info report, got {status} {got}"
    );
    assert!(
        got.get("ConnectivityInfo").is_none(),
        "case {case:?}: a report naming no connectivity information carries no endpoint \
         list, got {got}"
    );
}

/// The status the framework refuses a value outside a modelled bound at, before any
/// handler runs.
const MODELED_REFUSAL_STATUS: u16 = 400;

/// The bounds `ConnectivityInfo$portNumber` is declared with
/// (`smithy_query(query="shape",shape="ConnectivityInfo")`: `PortNumberInt`,
/// `range 0..65535`), so a value outside them is refused before any handler runs.
const PORT_MIN: i64 = 0;
const PORT_MAX: i64 = 65_535;

/// The emulator front door with only this service mounted, over ONE shared engine —
/// exactly how `core/router` assembles it.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Dispatch one signed request at a core device's connectivity-info route and return its
/// status and parsed JSON body.
pub(crate) async fn call(
    router: &MultiServiceRouter,
    method: &str,
    thing_name: &str,
    body: Vec<u8>,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://greengrass.{REGION}.amazonaws.com/greengrass/things/{thing_name}/\
             connectivityInfo"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/greengrass/aws4_request, \
                 SignedHeaders=host, Signature=00"
            ),
        )
        .header("content-type", "application/json")
        .body(Bytes::from(body))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or(Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    (status, value)
}

/// `UpdateConnectivityInfo` for `thing_name`, publishing `entries`.
pub(crate) async fn publish(
    router: &MultiServiceRouter,
    thing_name: &str,
    entries: &Value,
) -> (u16, Value) {
    let body = serde_json::to_vec(&json!({ "ConnectivityInfo": entries }))
        .expect("a serializable request body");
    call(router, "PUT", thing_name, body).await
}

/// `GetConnectivityInfo` for `thing_name`.
pub(crate) async fn read(router: &MultiServiceRouter, thing_name: &str) -> (u16, Value) {
    call(router, "GET", thing_name, Vec::new()).await
}

/// A string member of a JSON object, or the empty string when absent.
pub(crate) fn text(body: &Value, key: &str) -> String {
    body.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `ConnectivityInfo` list a body reports, or an empty list when it carries none.
pub(crate) fn reported(body: &Value) -> Vec<Value> {
    body.get("ConnectivityInfo")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Publish `entries` for `thing_name` and answer the version token the write reported,
/// having checked the status and that a token was named at all.
pub(crate) async fn published_version(
    router: &MultiServiceRouter,
    case: &str,
    thing_name: &str,
    entries: &Value,
) -> String {
    let (status, written) = publish(router, thing_name, entries).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from UpdateConnectivityInfo, got {status} {written}"
    );
    let version = text(&written, "Version");
    assert!(
        !version.is_empty(),
        "case {case:?}: expected a version token for the stored list, got {written}"
    );
    version
}

/// Endpoint lists a core device publishes, spanning what the rules generalise over: how
/// many endpoints one write carries, which of the four optional members each entry carries
/// (`smithy_query(query="shape",shape="ConnectivityInfo")`: `Id`, `HostAddress`,
/// `PortNumber`, `Metadata`), the form of the address, and both published bounds of the
/// port range.
fn endpoint_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "one endpoint carrying every member",
            json!([{
                "Id": "local-ipv4",
                "HostAddress": "192.168.1.14",
                "PortNumber": 8_883,
                "Metadata": "the LAN interface",
            }]),
        ),
        (
            "one endpoint carrying only what an endpoint needs",
            json!([{
                "Id": "dns-only",
                "HostAddress": "core.example.internal",
                "PortNumber": 8_883,
            }]),
        ),
        (
            "several endpoints, mixing address forms",
            json!([
                { "Id": "ipv4", "HostAddress": "10.0.3.7", "PortNumber": 8_883 },
                { "Id": "ipv6", "HostAddress": "fd00::1", "PortNumber": 8_884 },
                {
                    "Id": "dns",
                    "HostAddress": "laser-1.core.example.com",
                    "PortNumber": 443,
                    "Metadata": "reachable over the internet",
                },
            ]),
        ),
        (
            "an endpoint at the lowest port a broker can listen on",
            json!([{ "Id": "low", "HostAddress": "10.0.0.1", "PortNumber": 1 }]),
        ),
        (
            "an endpoint at the highest port the model admits",
            json!([{ "Id": "high", "HostAddress": "10.0.0.2", "PortNumber": PORT_MAX }]),
        ),
    ]
}

/// Core device thing names, spanning the form the model admits
/// (`smithy_query(query="operation",operation="GetConnectivityInfo")`:
/// `CoreDeviceThingName`, `length 1..128`) — a plain name, one carrying the separators an
/// IoT thing name admits, a single character, and one at the length bound.
fn thing_name_rows() -> Vec<(&'static str, String)> {
    vec![
        ("a plain thing name", "laser-1".to_string()),
        (
            "a thing name carrying every separator IoT admits",
            "fleet_a-core.1:probe".to_string(),
        ),
        ("a one-character thing name", "c".to_string()),
        (
            "a thing name at the modelled length bound",
            "c".repeat(128),
        ),
    ]
}

#[tokio::test]
async fn publishing_connectivity_info_replaces_it_and_a_read_reports_what_was_stored() {
    let router = emulator();
    let thing_name = "laser-1";

    // Nothing has been published for this core device yet, so the read reports that — and
    // asking does not bring a record into being.
    for attempt in 1..=2 {
        assert_no_connectivity_info(
            &router,
            &format!("nothing published, read {attempt}"),
            thing_name,
        )
        .await;
    }

    // Each row publishes OVER the list the previous row left, so the rule under test is
    // that the write REPLACES rather than adds: after every row the read reports exactly
    // that row's endpoints, entry for entry and member for member, and the version the
    // write answered is one no earlier write answered.
    let mut versions: Vec<String> = Vec::new();
    for (case, entries) in endpoint_rows() {
        let version = published_version(&router, case, thing_name, &entries).await;
        assert!(
            !versions.contains(&version),
            "case {case:?}: expected a version token no earlier write answered, got \
             {version:?} after {versions:?}"
        );
        versions.push(version);

        let expected = entries.as_array().cloned().unwrap_or_default();
        for attempt in 1..=2 {
            let (status, got) = read(&router, thing_name).await;
            assert_eq!(
                status, 200,
                "case {case:?} read {attempt}: expected 200 from GetConnectivityInfo, \
                 got {status} {got}"
            );
            let actual = reported(&got);
            assert_eq!(
                actual.len(),
                expected.len(),
                "case {case:?} read {attempt}: expected {} endpoint(s), got {got}",
                expected.len()
            );
            for (index, want) in expected.iter().enumerate() {
                let entry = actual.get(index).cloned().unwrap_or(Value::Null);
                // Every member the write carried is reported under the same wire name and
                // with the same value; a member the write omitted is not invented, except
                // `PortNumber`, which the model gives a default of 0.
                for (name, value) in want.as_object().expect("an entry is an object") {
                    assert_eq!(
                        entry.get(name),
                        Some(value),
                        "case {case:?} read {attempt} entry {index}: expected \
                         {name}={value}, got {entry}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn each_core_devices_connectivity_info_is_its_own() {
    let router = emulator();

    // Every thing name is published to in turn, each with a distinguishable address, and
    // then every one is read back: a store keyed by anything but the thing name — or a
    // write that replaced the wrong record — shows up as one device reporting another's
    // endpoints.
    let names = thing_name_rows();
    let mut expected: Vec<(&str, String, String)> = Vec::new();
    for (index, (case, thing_name)) in names.iter().enumerate() {
        let address = format!("10.0.{index}.1");
        let entries = json!([{
            "Id": format!("endpoint-{index}"),
            "HostAddress": address,
            "PortNumber": 8_883,
        }]);
        let version = published_version(&router, case, thing_name, &entries).await;
        expected.push((case, address, version));
    }

    let mut versions: Vec<&String> = Vec::new();
    for ((case, address, version), (_, thing_name)) in expected.iter().zip(&names) {
        let (status, got) = read(&router, thing_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetConnectivityInfo, got {status} {got}"
        );
        let entries = reported(&got);
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: expected exactly the one endpoint published for this core \
             device, got {got}"
        );
        assert_eq!(
            entries
                .first()
                .and_then(|entry| entry.get("HostAddress"))
                .and_then(Value::as_str),
            Some(address.as_str()),
            "case {case:?}: expected this core device's own address, got {got}"
        );
        assert!(
            !versions.contains(&version),
            "case {case:?}: expected a version token no other core device's write \
             answered, got {version:?}"
        );
        versions.push(version);
    }
}

#[tokio::test]
async fn a_list_that_is_not_connectivity_info_is_refused_and_stores_nothing() {
    let router = emulator();
    let thing_name = "laser-1";

    // (case, the list the write carries)
    //
    // Connectivity information is a list of ENDPOINTS, so a list that names none — and an
    // entry that leaves one of the three things an endpoint is named by unnamed — names no
    // endpoint: the rows span the ways that happens — the empty list, each of the three
    // absent in turn, each blank or zero where the wire cannot tell that from absent, and
    // one such entry among readable ones, which refuses the whole list.
    let rows: Vec<(&str, Value)> = vec![
        ("a list naming no endpoint at all", json!([])),
        (
            "an entry carrying no address",
            json!([{ "Id": "no-address", "PortNumber": 8_883 }]),
        ),
        (
            "an entry carrying no id",
            json!([{ "HostAddress": "10.0.0.1", "PortNumber": 8_883 }]),
        ),
        (
            "an entry carrying no port",
            json!([{ "Id": "no-port", "HostAddress": "10.0.0.1" }]),
        ),
        (
            "an entry carrying only a port",
            json!([{ "PortNumber": 8_883 }]),
        ),
        ("an entry carrying nothing at all", json!([{}])),
        (
            "an entry carrying an empty address",
            json!([{ "Id": "empty", "HostAddress": "", "PortNumber": 8_883 }]),
        ),
        (
            "an entry carrying a blank address",
            json!([{ "Id": "blank", "HostAddress": "   ", "PortNumber": 8_883 }]),
        ),
        (
            "an entry carrying a blank id",
            json!([{ "Id": " ", "HostAddress": "10.0.0.1", "PortNumber": 8_883 }]),
        ),
        (
            "an entry naming the port the default makes indistinguishable from none",
            json!([{ "Id": "zero-port", "HostAddress": "10.0.0.1", "PortNumber": PORT_MIN }]),
        ),
        (
            "one unreadable entry among readable ones",
            json!([
                { "Id": "good", "HostAddress": "10.0.0.1", "PortNumber": 8_883 },
                { "Id": "bad", "PortNumber": 8_884 },
            ]),
        ),
    ];

    // Nothing is published yet, so a refused write must leave this core device with no
    // endpoints: the refusal is decided on the request, before anything is stored.
    for (case, entries) in &rows {
        let (status, refused) = publish(&router, thing_name, entries).await;
        assert_eq!(
            (status, text(&refused, "Message").as_str()),
            (UNREADABLE_STATUS, UNREADABLE_BODY_MESSAGE),
            "case {case:?}: expected the unreadable-body refusal, got {status} {refused}"
        );
        assert_no_connectivity_info(
            &router,
            &format!("{case}: a refused write stores nothing"),
            thing_name,
        )
        .await;
    }

    // With endpoints published, a refused write leaves THAT list exactly as it was: the
    // refusal replaces nothing.
    let held = json!([{ "Id": "held", "HostAddress": "10.0.9.9", "PortNumber": 8_883 }]);
    let version = published_version(&router, "a published list", thing_name, &held).await;
    for (case, entries) in &rows {
        let (status, refused) = publish(&router, thing_name, entries).await;
        assert_eq!(
            (status, text(&refused, "Message").as_str()),
            (UNREADABLE_STATUS, UNREADABLE_BODY_MESSAGE),
            "case {case:?} over a published list: expected the unreadable-body refusal, \
             got {status} {refused}"
        );
        let (status, got) = read(&router, thing_name).await;
        assert_eq!(
            (status, reported(&got)),
            (200, held.as_array().cloned().unwrap_or_default()),
            "case {case:?}: expected the published list untouched, got {status} {got}"
        );
    }

    // And the refusals minted no version of their own: the list still reads back under
    // the one the accepted write answered.
    let again = published_version(&router, "a second accepted write", thing_name, &held).await;
    assert_ne!(
        again, version,
        "an accepted write mints a fresh version token, got {again:?} twice"
    );
}

#[tokio::test]
async fn a_port_outside_the_modelled_range_is_refused() {
    let router = emulator();
    let thing_name = "laser-1";

    // The bound is the model's own, so the framework refuses the value before any handler
    // runs: what this case pins is that the refusal happens at all and that nothing is
    // stored for it. One past each published bound.
    for (case, port) in [
        ("one below the lowest port", PORT_MIN - 1),
        ("one above the highest port", PORT_MAX + 1),
    ] {
        let entries = json!([{ "Id": "out-of-range", "HostAddress": "10.0.0.1", "PortNumber": port }]);
        let (status, refused) = publish(&router, thing_name, &entries).await;
        assert_eq!(
            status, MODELED_REFUSAL_STATUS,
            "case {case:?}: expected {MODELED_REFUSAL_STATUS} for a port outside the \
             modelled range, got {status} {refused}"
        );
        assert_no_connectivity_info(
            &router,
            &format!("{case}: a refused write stores nothing"),
            thing_name,
        )
        .await;
    }
}
