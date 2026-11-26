// SPDX-License-Identifier: CC0-1.0

//! WHICH address this service REPORTS as a Device Advisor endpoint, over every way a caller
//! can address the emulator and every address the emulator can be served at.
//!
//! The rule under test is one sentence: an operation that reports an endpoint a device DIALS
//! reports an address THIS emulator answers on — the `host:port` of a socket of this process,
//! which for a Device Advisor endpoint is the MQTT message broker a device under test
//! connects to — never a hostname in a DNS zone nothing here resolves.
//!
//! Both operations that report an endpoint are driven here — `GetEndpoint`, which vends it,
//! and `StartSuiteRun`, which reports and persists where its primary device connects — so the
//! two cannot drift: a client told an address by the start call is told the same address when
//! it asks where that device should connect.
//!
//! The tables span what the rule generalises over: the form of the address a client reaches
//! this emulator at (a loopback socket, a loopback address with no port, a loopback NAME, an
//! IPv6 literal, a private address, an address carried only on the request URI), the hosts
//! that name a service instead (this service's own endpoint host and an API endpoint host), a
//! request that addresses no host at all, and the bases this emulator can be SERVED at
//! ([`served_bases`]) — none of which moves the address reported, because the address is a
//! socket of this process rather than anything a request carried.
//!
//! Every case drives real HTTP requests through the same `MultiServiceRouter` the emulator
//! serves on a socket, and every reported address is DIALLED: a client that opens a
//! connection to it is accepted, which is what the member exists for.

use serde_json::json;
use service_com_amazonaws_iotdeviceadvisor_iotsenateservice as iotdeviceadvisor;

use ::service_common::data_plane_addressing as addressing;

use super::suite_lifecycle::{Answer, Emulator, ROLE_ARN};

/// The region every case signs for unless it is varying the region itself.
const REGION: &str = "us-west-2";

/// A thing ARN for a device under test (`arn:aws:iot:<region>:<account>:thing/<name>`).
fn thing_arn(name: &str) -> String {
    format!(
        "arn:aws:iot:{REGION}:{}:thing/{name}",
        emulator_server::emulator_account_id()
    )
}

/// How one case addresses this emulator.
struct Addressing {
    /// What this row is called in an assertion message.
    case: &'static str,
    /// The RAW bytes of the `Host` header the request carries, or `None` for a request that
    /// sends no `Host` header at all (as an HTTP/2 request does).
    host: Option<&'static [u8]>,
    /// The authority the request URI carries in absolute form, or `None` for an
    /// origin-form request URI, which carries none.
    authority: Option<&'static str>,
}

/// The ways a caller reaches this emulator — none of which moves the address it is reported.
fn addressings() -> Vec<Addressing> {
    vec![
        Addressing {
            case: "the emulator's own socket",
            host: Some(b"127.0.0.1:9297"),
            authority: None,
        },
        Addressing {
            case: "a loopback address with no port",
            host: Some(b"127.0.0.1"),
            authority: None,
        },
        Addressing {
            case: "a loopback name with a port",
            host: Some(b"localhost:9297"),
            authority: None,
        },
        Addressing {
            case: "an IPv6 literal with a port",
            host: Some(b"[::1]:9297"),
            authority: None,
        },
        Addressing {
            case: "a private address with a port",
            host: Some(b"192.168.1.10:4566"),
            authority: None,
        },
        Addressing {
            case: "an address carried only on the request URI",
            host: None,
            authority: Some("127.0.0.1:9297"),
        },
        Addressing {
            case: "this service's own endpoint host",
            host: Some(b"iotdeviceadvisor.us-west-2.amazonaws.com"),
            authority: None,
        },
        Addressing {
            case: "an API endpoint host",
            host: Some(b"api.iotdeviceadvisor.us-west-2.amazonaws.com"),
            authority: None,
        },
        Addressing {
            case: "no host at all",
            host: None,
            authority: None,
        },
    ]
}

/// `GET /endpoint`, addressed the way `addressing` asks, naming the device `query` names.
async fn get_endpoint(
    emulator: &Emulator,
    addressing: &Addressing,
    query: &[(&str, &str)],
) -> Answer {
    let mut path = "/endpoint".to_string();
    for (index, (name, value)) in query.iter().enumerate() {
        path.push(if index == 0 { '?' } else { '&' });
        path.push_str(name);
        path.push('=');
        path.push_str(&percent_encode(value));
    }
    emulator
        .call_addressed("GET", &path, REGION, addressing.host, addressing.authority)
        .await
}

/// A query value as an SDK sends it: everything outside the unreserved set escaped, which
/// for an ARN is the `:` and `/` it is built from.
fn percent_encode(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                c.to_string()
            } else {
                c.to_string()
                    .bytes()
                    .map(|byte| format!("%{byte:02X}"))
                    .collect()
            }
        })
        .collect()
}

/// Create one suite definition and hand back its id.
async fn definition(emulator: &Emulator, name: &str) -> String {
    let created = emulator
        .create_definition(
            &json!({
                "suiteDefinitionName": name,
                "rootGroup": "g",
                "devicePermissionRoleArn": ROLE_ARN,
            }),
            REGION,
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: expected HTTP 200 from CreateSuiteDefinition, got {} {}",
        created.status, created.body
    );
    created.string("suiteDefinitionId")
}

/// The `endpoint` the store holds for the suite run `run_id`.
fn persisted_run_endpoint(emulator: &Emulator, run_id: &str) -> Option<String> {
    let state = iotdeviceadvisor::state::service_state(&*emulator.engine)
        .expect("the service's store is reachable from the shared engine");
    let store = state.lock();
    store.runs.get(run_id).map(|run| run.endpoint.clone())
}

/// Assert `endpoint` is an address THIS emulator answers on: an authority written with
/// address characters alone, carrying the port a client dials, whose host names an interface
/// this process's listeners are bound on — and which ACCEPTS a connection, which is the whole
/// reason a caller is handed the value.
pub(crate) async fn assert_reachable_address(case: &str, endpoint: &str) {
    assert!(
        endpoint
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')),
        "case {case:?}: the reported endpoint must be an address, got {endpoint:?}"
    );
    assert!(
        addressing::addressed_port(Some(endpoint)).is_some(),
        "case {case:?}: the reported endpoint must carry the port a client dials, got \
         {endpoint:?}"
    );
    assert!(
        addressing::host_names_the_bound_interface(endpoint),
        "case {case:?}: the reported endpoint must name an interface this emulator is bound \
         on, got {endpoint:?}"
    );
    // The zones AWS publishes its own endpoints in, as this tree reads them
    // (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`): a name in one of
    // them resolves nowhere near this process, or at genuine AWS infrastructure.
    for aws_zone in addressing::AWS_ENDPOINT_DNS_SUFFIXES {
        assert!(
            !addressing::host_only(endpoint).ends_with(aws_zone),
            "case {case:?}: the reported endpoint must not name a host AWS serves \
             ({aws_zone}), got {endpoint:?}"
        );
    }
    // The proof that the address is this emulator's own: a client dials it and is accepted.
    // The host the value carries is an address of the loopback interface the core binds on
    // (asserted above), so the connection names that interface by its own literal and the
    // port the value carries, and never leaves this machine.
    let port = u16::try_from(addressing::addressed_port(Some(endpoint)).unwrap_or_default())
        .unwrap_or_default();
    let dialled = ::tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
    assert!(
        dialled.is_ok(),
        "case {case:?}: a client must be able to open a connection to the reported endpoint \
         {endpoint:?}, got {:?}",
        dialled.err()
    );
}

/// Every endpoint this service reports is an address this emulator answers on, whichever
/// address the request reached this emulator on — whether it names a device or none, and
/// whichever operation reports it. It is ONE address: an address names the socket a client
/// dials, so nothing a request carries moves it.
#[tokio::test]
async fn a_reported_endpoint_is_an_address_this_emulator_answers_on() {
    let emulator = Emulator::new();

    for addressing in addressings() {
        let case = addressing.case;
        let thing = thing_arn(&format!("announced-{}", case.replace(' ', "-")));

        // The question a request that names no device asks.
        let no_device = get_endpoint(&emulator, &addressing, &[]).await;
        assert_eq!(
            no_device.status, 200,
            "case {case:?}: expected HTTP 200 from GetEndpoint, got {} {}",
            no_device.status, no_device.body
        );
        let no_device = no_device.string("endpoint");

        // The device-level question: the same request naming a device under test.
        let device_level = get_endpoint(&emulator, &addressing, &[("thingArn", &thing)]).await;
        assert_eq!(
            device_level.status, 200,
            "case {case:?}: expected HTTP 200 from GetEndpoint, got {} {}",
            device_level.status, device_level.body
        );
        let device_level = device_level.string("endpoint");

        // And the run that starts against that very device, addressed the same way.
        let definition_id = definition(&emulator, &format!("announced-suite-{case}")).await;
        let started = emulator
            .call_addressed_with_body(
                "POST",
                &format!("/suiteDefinitions/{definition_id}/suiteRuns"),
                REGION,
                addressing.host,
                addressing.authority,
                Some(json!({
                    "suiteRunConfiguration": { "primaryDevice": { "thingArn": thing } }
                })),
            )
            .await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected HTTP 200 from StartSuiteRun, got {} {}",
            started.status, started.body
        );
        let reported = started.string("endpoint");
        let run_id = started.string("suiteRunId");

        assert_eq!(
            reported, device_level,
            "case {case:?}: StartSuiteRun must report the endpoint GetEndpoint vends, got \
             {reported:?} and {device_level:?}"
        );
        assert_eq!(
            persisted_run_endpoint(&emulator, &run_id).as_deref(),
            Some(reported.as_str()),
            "case {case:?}: the run must persist the endpoint its start reported, got {:?}",
            persisted_run_endpoint(&emulator, &run_id)
        );

        assert_reachable_address(case, &no_device).await;
        assert_reachable_address(case, &device_level).await;
        assert_eq!(
            no_device, device_level,
            "case {case:?}: the endpoint is the address this emulator answers on, which a \
             device identity cannot move, got {device_level:?} and {no_device:?}"
        );
    }
}

/// The address reported is the socket this emulator serves the device wire on: the MQTT
/// message broker a device under test connects to, opened by the call that asks where to
/// connect and kept from then on, so every later call is answered that same socket.
#[tokio::test]
async fn the_reported_endpoint_is_the_socket_this_emulator_serves_the_device_wire_on() {
    let emulator = Emulator::new();
    let broker = |label: &str| {
        ::service_common::iot_mqtt_broker::published_broker_endpoint(&*emulator.engine)
            .unwrap_or_else(|| panic!("case {label:?}: the broker's socket must be open"))
    };

    assert_eq!(
        ::service_common::iot_mqtt_broker::published_broker_endpoint(&*emulator.engine),
        None,
        "case \"before any call\": no socket is open until an endpoint is asked for"
    );

    let own = Addressing {
        case: "the emulator's own socket",
        host: Some(b"127.0.0.1:9297"),
        authority: None,
    };
    let vended = get_endpoint(&emulator, &own, &[]).await.string("endpoint");
    assert_eq!(
        vended,
        broker("the first call"),
        "case \"the first call\": the endpoint reported must be the socket the device wire \
         is served on, got {vended:?}"
    );
    assert_reachable_address("the first call", &vended).await;

    // Idempotent: the first socket opened is the one kept, so a client may hold the address.
    let again = get_endpoint(&emulator, &own, &[]).await.string("endpoint");
    assert_eq!(
        again, vended,
        "case \"a second call\": the endpoint must be the socket already open, got {again:?} \
         and {vended:?}"
    );
    assert_eq!(
        broker("a second call"),
        vended,
        "case \"a second call\": a second call must not open a second socket"
    );
}

/// The address reported is a function of this emulator alone: asking twice through one
/// address answers one address, and every address a caller can reach this emulator by is
/// answered the same one.
#[tokio::test]
async fn the_reported_endpoint_does_not_follow_the_address_the_request_reached() {
    let emulator = Emulator::new();
    let thing = thing_arn("two-endpoints-device");
    let query = [("thingArn", thing.as_str())];

    let mut seen: Vec<(&str, String)> = Vec::new();
    for addressing in addressings() {
        let case = addressing.case;
        let announced = get_endpoint(&emulator, &addressing, &query)
            .await
            .string("endpoint");
        let again = get_endpoint(&emulator, &addressing, &query)
            .await
            .string("endpoint");
        assert_eq!(
            again, announced,
            "case {case:?}: asking twice through one address must answer one endpoint, got \
             {announced:?} and {again:?}"
        );
        assert_reachable_address(case, &announced).await;

        for (other_case, other) in &seen {
            assert_eq!(
                &announced, other,
                "case {case:?}: must answer the endpoint reported for case {other_case:?}, \
                 got {announced:?} and {other:?}"
            );
        }
        seen.push((case, announced));
    }
}

/// The address reported does not follow the region the request is SIGNED for either: a
/// socket of this process belongs to no region, so one device asked about in several regions
/// is answered the one address it can be reached at.
#[tokio::test]
async fn a_reported_endpoint_does_not_follow_the_region_the_request_is_signed_for() {
    let emulator = Emulator::new();
    let own = Addressing {
        case: "the emulator's own socket",
        host: Some(b"127.0.0.1:9297"),
        authority: None,
    };

    // Regions of several shapes, including a GovCloud one and a four-label one.
    let mut seen: Vec<(&str, String)> = Vec::new();
    for region in ["us-east-1", "eu-central-1", "ap-southeast-4", "us-gov-west-1"] {
        let answer = emulator
            .call_addressed("GET", "/endpoint", region, own.host, own.authority)
            .await;
        assert_eq!(
            answer.status, 200,
            "case {region:?}: expected HTTP 200 from GetEndpoint, got {} {}",
            answer.status, answer.body
        );
        let endpoint = answer.string("endpoint");
        assert_reachable_address(region, &endpoint).await;
        for (other_region, other) in &seen {
            assert_eq!(
                &endpoint, other,
                "case {region:?}: the address this emulator answers on is the same in every \
                 region, got {endpoint:?} for {region} and {other:?} for {other_region}"
            );
        }
        seen.push((region, endpoint));
    }
}

/// An endpoint is a REPORT: asking for one neither mints a record of this service's nor
/// changes one.
#[tokio::test]
async fn asking_for_an_endpoint_changes_nothing() {
    let emulator = Emulator::new();
    let thing = thing_arn("read-only-device");

    for addressing in addressings() {
        for query in [vec![], vec![("thingArn", thing.as_str())]] {
            let answer = get_endpoint(&emulator, &addressing, &query).await;
            assert_eq!(
                answer.status, 200,
                "case {:?}: expected HTTP 200 from GetEndpoint, got {} {}",
                addressing.case, answer.status, answer.body
            );
        }
    }

    let state = iotdeviceadvisor::state::service_state(&*emulator.engine)
        .expect("the service's store is reachable from the shared engine");
    let store = state.lock();
    assert!(
        store.definitions.is_empty() && store.runs.is_empty() && store.tags.is_empty(),
        "asking for an endpoint must store nothing, got {} definitions, {} runs and {} \
         tag rows",
        store.definitions.len(),
        store.runs.len(),
        store.tags.len()
    );
}

/// One row of the served-address table: where this emulator ANSWERS its own HTTP surface.
struct ServedBase {
    /// What this row is called in an assertion message.
    case: &'static str,
    /// The base URL `OperationContext::endpoint` reports — what the core records when the
    /// data port starts — or `None` for an emulator served on no port at all.
    base: Option<&'static str>,
}

/// The bases an emulator can be served at: a socket, an IPv6 socket, a routable address, the
/// loopback name, a name an operator serves it under, a name with no port, and the base an
/// emulator served on no port at all reports. The endpoint this service reports is the socket
/// it serves the device wire on, so none of these is what it answers.
fn served_bases() -> Vec<ServedBase> {
    vec![
        ServedBase {
            case: "served on no port",
            base: None,
        },
        ServedBase {
            case: "a loopback socket",
            base: Some("https://127.0.0.1:42899"),
        },
        ServedBase {
            case: "an IPv6 loopback socket",
            base: Some("https://[::1]:9297"),
        },
        ServedBase {
            case: "a routable address",
            base: Some("https://192.168.1.10:4566"),
        },
        ServedBase {
            case: "the loopback name with a port",
            base: Some("https://localhost:9297"),
        },
        ServedBase {
            case: "a name an operator serves it under",
            base: Some("https://emulator.internal:8443"),
        },
        ServedBase {
            case: "a name with no port",
            base: Some("https://host.docker.internal"),
        },
    ]
}

/// This service's emulator, served at the address one row of the table above names.
struct ServedEmulator {
    router: ::smithy_server_runtime::MultiServiceRouter,
    engine: ::std::sync::Arc<engine::InMemoryEngine>,
}

impl ServedEmulator {
    /// The emulator answering at `base`, set the way the core sets it when the data port
    /// starts; `None` leaves it the base an emulator served on no port reports.
    fn new(base: Option<&str>) -> Self {
        let engine = ::std::sync::Arc::new(engine::InMemoryEngine::new());
        let mut router = ::smithy_server_runtime::MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(iotdeviceadvisor::EmulatedService::new(
            ::std::sync::Arc::clone(&engine),
        ));
        if let Some(base) = base {
            router.set_url_base(
                ::smithy_server_runtime::EndpointUrl::parse(base)
                    .unwrap_or_else(|| panic!("{base:?} is a usable endpoint base")),
            );
        }
        Self { router, engine }
    }

    /// One request, signed in scope for [`REGION`] (authentication is off) and addressed to
    /// a socket of this machine, so what moves between rows is only WHERE the emulator
    /// answers.
    async fn call(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> Answer {
        let payload = body.map_or_else(bytes::Bytes::new, |value| {
            bytes::Bytes::from(serde_json::to_vec(&value).expect("the request body serializes"))
        });
        let request = http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:9297")
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iotdeviceadvisor/\
                     aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        Answer {
            status,
            body,
            error_code,
        }
    }

    /// `GET /endpoint`, naming the device `query` names.
    async fn endpoint(&self, query: &[(&str, &str)]) -> String {
        let mut path = "/endpoint".to_string();
        for (index, (name, value)) in query.iter().enumerate() {
            path.push(if index == 0 { '?' } else { '&' });
            path.push_str(name);
            path.push('=');
            path.push_str(&percent_encode(value));
        }
        let answer = self.call("GET", &path, None).await;
        assert_eq!(
            answer.status, 200,
            "expected HTTP 200 from GetEndpoint, got {} {}",
            answer.status, answer.body
        );
        answer.string("endpoint")
    }
}

/// Every endpoint this service reports is the socket this emulator serves the device wire on
/// — never the base its own HTTP surface is served at — whichever base that is, whatever the
/// request names, and for every operation that reports one.
#[tokio::test]
async fn a_reported_endpoint_is_this_emulators_own_socket_whatever_base_it_is_served_at() {
    for ServedBase { case, base } in served_bases() {
        let emulator = ServedEmulator::new(base);
        let thing = thing_arn("served-base-device");

        let no_device = emulator.endpoint(&[]).await;
        let named_device = emulator.endpoint(&[("thingArn", &thing)]).await;

        let created = emulator
            .call(
                "POST",
                "/suiteDefinitions",
                Some(json!({
                    "suiteDefinitionConfiguration": {
                        "suiteDefinitionName": "served-base-suite",
                        "rootGroup": "g",
                        "devicePermissionRoleArn": ROLE_ARN,
                    }
                })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from CreateSuiteDefinition, got {} {}",
            created.status, created.body
        );
        let definition_id = created.string("suiteDefinitionId");
        let started = emulator
            .call(
                "POST",
                &format!("/suiteDefinitions/{definition_id}/suiteRuns"),
                Some(json!({
                    "suiteRunConfiguration": { "primaryDevice": { "thingArn": thing } }
                })),
            )
            .await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected HTTP 200 from StartSuiteRun, got {} {}",
            started.status, started.body
        );
        let reported = started.string("endpoint");

        let socket = ::service_common::iot_mqtt_broker::published_broker_endpoint(
            &*emulator.engine,
        )
        .unwrap_or_else(|| panic!("case {case:?}: the broker's socket must be open"));
        for (reporter, endpoint) in [
            ("GetEndpoint, no device", &no_device),
            ("GetEndpoint, a device", &named_device),
            ("StartSuiteRun", &reported),
        ] {
            assert_reachable_address(&format!("{case}: {reporter}"), endpoint).await;
            assert_eq!(
                endpoint, &socket,
                "case {case:?}: {reporter} must report the socket the device wire is served \
                 on ({socket:?}), got {endpoint:?}"
            );
        }

        let again = emulator.endpoint(&[("thingArn", &thing)]).await;
        assert_eq!(
            again, named_device,
            "case {case:?}: asking twice must answer one address, got {named_device:?} and \
             {again:?}"
        );
    }
}
