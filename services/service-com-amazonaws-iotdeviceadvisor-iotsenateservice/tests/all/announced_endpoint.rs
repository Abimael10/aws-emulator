// SPDX-License-Identifier: CC0-1.0

//! WHICH endpoint this service REPORTS, over every way a caller can address the emulator.
//!
//! The rule under test is one sentence: `endpoint` is the connection target the device under
//! test is handed and then dials, so an operation reporting one reports the address THIS
//! emulator answers on — the `host[:port]` of the one endpoint its whole surface is served
//! at — and never a host AWS publishes its own endpoints at, whichever address the asking
//! request reached this emulator on and whichever account, region or device it asks about.
//!
//! Both operations that report an endpoint are driven here — `GetEndpoint`, which vends it,
//! and `StartSuiteRun`, which reports and persists where its primary device connects
//! — so the two cannot drift: a client told an address by the start call is told the same
//! address when it asks where that device should connect.
//!
//! The table spans what the rule generalises over: the form of the address a client
//! reaches this emulator at (a loopback socket, a loopback address with no port, a loopback
//! NAME, an IPv6 literal, a private address, an address carried only on the request URI),
//! the hosts that name a service instead (this service's own endpoint host and an API
//! endpoint host), and a request that addresses no host at all; each crossed with a request
//! that names a device and one that names none. WHICH address is reported — the endpoint
//! this process is SERVED at, not the address one request carried — is the
//! [`served_bases`] table.
//!
//! Every case drives real HTTP requests through the same `MultiServiceRouter` the emulator
//! serves on a socket.

use serde_json::json;
use service_com_amazonaws_iotdeviceadvisor_iotsenateservice as iotdeviceadvisor;

use super::suite_lifecycle::{Answer, Emulator, ROLE_ARN};

/// The region every case signs for.
const REGION: &str = "us-west-2";

/// The address this emulator answers on, which is what every endpoint it reports carries:
/// the `host[:port]` of its own endpoint — the base an emulator served on no port, as the
/// router this suite drives is, reports.
fn served_address() -> String {
    iotdeviceadvisor::state::announced_endpoint(::smithy_server_runtime::UNSERVED_URL_BASE)
}

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

/// The ways a caller reaches this emulator — none of which is a value of the endpoint it is
/// reported.
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

/// Assert `endpoint` is the address this emulator answers on — the only address a device
/// handed it can reach — and names no host AWS publishes its own endpoints at, nor any AWS
/// identifier a caller could read off it.
fn assert_announced_address(case: &str, endpoint: &str) {
    let served = served_address();
    assert_eq!(
        endpoint, served,
        "case {case:?}: the reported endpoint must be the address this emulator answers on \
         ({served:?}), got {endpoint:?}"
    );
    // The zones AWS publishes its own endpoints in, as this tree reads them
    // (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`).
    for aws_zone in ::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES {
        assert!(
            !endpoint.ends_with(aws_zone),
            "case {case:?}: the reported endpoint must not name a host AWS serves \
             ({aws_zone}), got {endpoint:?}"
        );
    }
    assert!(
        !endpoint.contains(emulator_server::emulator_account_id()),
        "case {case:?}: the reported endpoint is an address, so it carries no account id, \
         got {endpoint:?}"
    );
}

/// Every endpoint this service reports is the address this emulator answers on, whichever
/// address the request reached this emulator on — whether it names a
/// device or none, and whichever operation reports it.
#[tokio::test]
async fn a_reported_endpoint_is_this_emulators_address_whatever_address_the_request_reached() {
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
        let device_level = get_endpoint(&emulator, &addressing, &[("thingArn", &thing)]).await;        assert_eq!(
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
            "case {case:?}: StartSuiteRun must report the endpoint GetEndpoint vends \
             for the run's primary device, got {reported:?} and {device_level:?}"
        );
        assert_eq!(
            persisted_run_endpoint(&emulator, &run_id).as_deref(),
            Some(reported.as_str()),
            "case {case:?}: the run must persist the endpoint its start reported, got {:?}",
            persisted_run_endpoint(&emulator, &run_id)
        );

        assert_announced_address(case, &no_device);
        assert_announced_address(case, &device_level);
        assert_eq!(
            no_device, device_level,
            "case {case:?}: one endpoint serves every device here, so naming a device must \
             be answered the address a request naming none is, got {device_level:?} and \
             {no_device:?}"
        );
    }
}

/// The endpoint reported is a function of the account, the region and the device alone: one
/// device asked about through any of the addresses above is answered the SAME host every
/// time.
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
        assert_announced_address(case, &announced);

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

/// The address reported names no region: this process serves one endpoint for every region
/// and resolves each request to the region its credential scope names, so the region a
/// request signs for moves the ARNs it mints and not where its device connects.
#[tokio::test]
async fn a_reported_endpoint_is_the_same_address_in_every_region() {
    let emulator = Emulator::new();
    let own = Addressing {
        case: "the emulator's own socket",
        host: Some(b"127.0.0.1:9297"),
        authority: None,
    };

    // Regions of several shapes, including a GovCloud one and the longest AWS publishes.
    for region in ["us-east-1", "eu-central-1", "ap-southeast-4", "us-gov-west-1"] {
        let answer = emulator
            .call_addressed("GET", "/endpoint", region, own.host, own.authority)
            .await;
        assert_eq!(
            answer.status, 200,
            "case {region:?}: expected HTTP 200 from GetEndpoint, got {} {}",
            answer.status, answer.body
        );
        assert_announced_address(region, &answer.string("endpoint"));
    }
}

/// An endpoint is a REPORT: asking for one neither mints a record nor changes one.
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

/// One row of the served-address table: where this emulator ANSWERS, and the address every
/// endpoint it reports then carries.
struct ServedBase {
    /// What this row is called in an assertion message.
    case: &'static str,
    /// The base URL `OperationContext::endpoint` reports — what the core records when the
    /// data port starts — or `None` for an emulator served on no port at all.
    base: Option<&'static str>,
    /// The address every endpoint this row reports must carry, written out rather than
    /// derived, so the table states the rule instead of restating the code.
    address: &'static str,
}

/// The bases an emulator can be served at, and the address each makes it report: the
/// `host[:port]` of that base, with no scheme and no path, because what a reported endpoint
/// carries is the address a client dials — and the base an emulator served on no port
/// reports is a host that never resolves (`https://emulator.invalid`, RFC 6761), which is
/// the honest answer for a process no client can reach over a socket.
fn served_bases() -> Vec<ServedBase> {
    vec![
        ServedBase {
            case: "served on no port",
            base: None,
            address: "emulator.invalid",
        },
        ServedBase {
            case: "a loopback socket",
            base: Some("https://127.0.0.1:42899"),
            address: "127.0.0.1:42899",
        },
        ServedBase {
            case: "an IPv6 loopback socket",
            base: Some("https://[::1]:9297"),
            address: "[::1]:9297",
        },
        ServedBase {
            case: "a routable address",
            base: Some("https://192.168.1.10:4566"),
            address: "192.168.1.10:4566",
        },
        ServedBase {
            case: "the loopback name with a port",
            base: Some("https://localhost:9297"),
            address: "localhost:9297",
        },
        ServedBase {
            case: "a name an operator serves it under",
            base: Some("https://emulator.internal:8443"),
            address: "emulator.internal:8443",
        },
        ServedBase {
            // No port, so the address names none either: an `https` URL without one is
            // dialled on 443, which is what a client appending nothing to this address does.
            case: "a name with no port",
            base: Some("https://host.docker.internal"),
            address: "host.docker.internal",
        },
    ]
}

/// This service's emulator, served at the address one row of the table above names.
struct ServedEmulator {
    router: ::smithy_server_runtime::MultiServiceRouter,
}

impl ServedEmulator {
    /// The emulator answering at `base`, set the way the core sets it when the data port
    /// starts; `None` leaves it the base an emulator served on no port reports.
    fn new(base: Option<&str>) -> Self {
        let engine = ::std::sync::Arc::new(engine::InMemoryEngine::new());
        let mut router = ::smithy_server_runtime::MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(iotdeviceadvisor::EmulatedService::new(engine));
        if let Some(base) = base {
            router.set_url_base(
                ::smithy_server_runtime::EndpointUrl::parse(base)
                    .unwrap_or_else(|| panic!("{base:?} is a usable endpoint base")),
            );
        }
        Self { router }
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

/// Assert `endpoint` is `address` — where this emulator answers for the row under test —
/// and names no host AWS publishes its own endpoints at.
fn assert_reports(case: &str, what: &str, endpoint: &str, address: &str) {
    assert_eq!(
        endpoint, address,
        "case {case:?}: {what} must be the address this emulator answers on ({address:?}), \
         got {endpoint:?}"
    );
    // The zones AWS publishes its own endpoints in, as this tree reads them
    // (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`).
    for aws_zone in ::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES {
        assert!(
            !endpoint.ends_with(aws_zone),
            "case {case:?}: {what} must not name a host AWS serves ({aws_zone}), got \
             {endpoint:?}"
        );
    }
}

/// Every endpoint this service reports is the address this emulator is SERVED at — so a
/// device handed one is pointed back at this process — whichever address that is, whatever
/// the request names, and for every operation that reports one.
#[tokio::test]
async fn a_reported_endpoint_is_the_address_this_emulator_is_served_at() {
    for ServedBase {
        case,
        base,
        address,
    } in served_bases()
    {
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

        assert_reports(case, "the endpoint vended for no device", &no_device, address);
        assert_reports(
            case,
            "the endpoint vended for a named device",
            &named_device,
            address,
        );
        assert_reports(
            case,
            "the endpoint a suite run reports",
            &reported,
            address,
        );

        let again = emulator.endpoint(&[("thingArn", &thing)]).await;
        assert_eq!(
            again, named_device,
            "case {case:?}: asking twice must answer one address, got {named_device:?} and \
             {again:?}"
        );
    }
}

