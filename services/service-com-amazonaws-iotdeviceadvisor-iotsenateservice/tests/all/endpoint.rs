// SPDX-License-Identifier: CC0-1.0

//! `GetEndpoint` over the wire: which host Device Advisor reports, for which request.
//!
//! The rule under test is one sentence: the endpoint is the connection target the device
//! under test dials, so it is a DNS HOSTNAME of AWS's documented Device Advisor form — an
//! endpoint-id label derived from the account, the region and the device named, then the
//! plane's labels and that region — rooted in the DNS zone THIS emulator answers in and
//! inside the modelled `Endpoint` length bound, for every request that is answered at all,
//! while a device identity whose ARN is not one for the member carrying it is refused with
//! the modelled `ValidationException`. AWS vends a host per account and per device, so the
//! region a request signs for and the device it names are what move the answer, and every
//! operation that reports one — `GetEndpoint` and the suite-run start that reports where
//! its primary device connects — reports the same host for the same device.
//!
//! Every case below drives a real HTTP request through the same `MultiServiceRouter` the
//! emulator serves on a socket. The tables span what the rule generalises over: the
//! signing region (several shapes, plus one that is not a region at all), which of the
//! three device members name the device and how many of them, both `AuthenticationMethod`
//! values the model enumerates, and the ways an ARN can fail to be one.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotdeviceadvisor_iotsenateservice as iotdeviceadvisor;
use smithy_server_runtime::MultiServiceRouter;

use iotdeviceadvisor::state::endpoint_addressing::{
    MAX_ENDPOINT_LEN, MIN_ENDPOINT_LEN, PLANE_LABELS,
};
use iotdeviceadvisor::state::DeviceIdentity;

/// The host this emulator reports in `region` for the device `device` names: the derivation
/// every reporting operation shares (`state::endpoint_for`), read off the base an emulator
/// served on no port — as the router this suite drives is — reports.
fn announced_host(region: &str, device: &DeviceIdentity<'_>) -> String {
    iotdeviceadvisor::state::endpoint_for(
        smithy_server_runtime::UNSERVED_URL_BASE,
        region,
        device,
    )
}

/// The host a request that names NO device is answered in `region`: the account-level
/// question, which AWS answers with a host of its own.
fn account_level_host(region: &str) -> String {
    announced_host(region, &DeviceIdentity::default())
}

/// A thing ARN for a device under test (`arn:aws:iot:<region>:<account>:thing/<name>`).
fn thing_arn(name: &str) -> String {
    format!("arn:aws:iot:us-east-1:000000000000:thing/{name}")
}

/// An IoT client-certificate ARN (`arn:aws:iot:<region>:<account>:cert/<id>`).
fn certificate_arn(id: &str) -> String {
    format!("arn:aws:iot:us-east-1:000000000000:cert/{id}")
}

/// An IAM role ARN — a global service's ARN, so its region field is empty.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::000000000000:role/{name}")
}

/// The emulator, assembled in-process with authentication off: this suite asserts
/// behaviour, and signature verification is the framework's own concern.
struct Emulator {
    router: MultiServiceRouter,
    /// The one shared engine the handlers write through, kept so a case can read the
    /// endpoint a suite run PERSISTED and compare it with the vended one.
    engine: Arc<engine::InMemoryEngine>,
}

/// One response, reduced to what a client branches on.
struct Answer {
    status: u16,
    body: Value,
    /// The restJson1 error code (`x-amzn-errortype`), when the answer carries one.
    error_code: Option<String>,
}

impl Answer {
    fn string(&self, name: &str) -> String {
        self.body[name]
            .as_str()
            .unwrap_or_else(|| panic!("member {name} is not a string in {}", self.body))
            .to_string()
    }
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(iotdeviceadvisor::EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Send one request, signed (in scope only — authentication is off) for `region`, so
    /// the handler resolves the same region a real SDK client would.
    async fn call(
        &self,
        method: &str,
        path_and_query: &str,
        region: &str,
        body: Option<Value>,
    ) -> Answer {
        let payload = body.map_or_else(bytes::Bytes::new, |value| {
            bytes::Bytes::from(serde_json::to_vec(&value).expect("the request body serializes"))
        });
        let request = http::Request::builder()
            .method(method)
            .uri(path_and_query)
            .header("host", "api.iotdeviceadvisor.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/iotdeviceadvisor/\
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
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Answer {
            status,
            body,
            error_code,
        }
    }

    /// `GET /endpoint` with the query members this request carries.
    async fn get_endpoint(&self, query: &[(&str, &str)], region: &str) -> Answer {
        let mut path = "/endpoint".to_string();
        for (index, (name, value)) in query.iter().enumerate() {
            path.push(if index == 0 { '?' } else { '&' });
            path.push_str(name);
            path.push('=');
            path.push_str(&percent_encode(value));
        }
        self.call("GET", &path, region, None).await
    }

    /// The `endpoint` the store holds on the suite run `run_id` names, if it holds that run.
    fn persisted_run_endpoint(&self, run_id: &str) -> Option<String> {
        let state = iotdeviceadvisor::state::service_state(&*self.engine)
            .expect("the service's store is reachable from the shared engine");
        let store = state.lock();
        store.runs.get(run_id).map(|run| run.endpoint.clone())
    }
}

/// A query value as an SDK sends it: everything outside the unreserved set escaped, which
/// for an ARN is the `:` and `/` it is built from.
pub(crate) fn percent_encode(value: &str) -> String {
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

/// Assert `endpoint` is a Device Advisor hostname of this emulator in `region`: inside the
/// modelled `Endpoint` length bound, written with hostname characters alone, naming the
/// plane and that region, and in no zone AWS serves its own endpoints in — which a client
/// handed one would resolve away from this process.
fn assert_device_advisor_host(case: &str, endpoint: &str, region: &str) {
    assert!(
        (MIN_ENDPOINT_LEN..=MAX_ENDPOINT_LEN).contains(&endpoint.len()),
        "case {case:?}: the endpoint must satisfy the modelled len \
         {MIN_ENDPOINT_LEN}..{MAX_ENDPOINT_LEN}, got {} in {endpoint:?}",
        endpoint.len()
    );
    assert!(
        endpoint
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':')),
        "case {case:?}: the endpoint must be a hostname, got {endpoint:?}"
    );
    let [plane, device_plane] = PLANE_LABELS;
    assert!(
        endpoint.contains(&format!(".{plane}.{device_plane}.{region}.")),
        "case {case:?}: the endpoint must name the Device Advisor plane in {region:?}, got \
         {endpoint:?}"
    );
    // The zones AWS publishes its own endpoints in, as this tree reads them
    // (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`).
    for aws_zone in ::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES {
        assert!(
            !::service_common::data_plane_addressing::host_only(endpoint).ends_with(aws_zone),
            "case {case:?}: the endpoint must not name a host AWS serves ({aws_zone}), got \
             {endpoint:?}"
        );
    }
}

/// A request that names no device is answered the account-level Device Advisor host of the
/// region it is signed for, the same host every time, and a host of its own per region.
#[tokio::test]
async fn a_request_naming_no_device_is_answered_the_account_level_host() {
    let emulator = Emulator::new();

    // Regions of several shapes, including the longest AWS publishes and a GovCloud one.
    let mut seen: Vec<(&str, String)> = Vec::new();
    for region in [
        "us-east-1",
        "us-west-2",
        "eu-central-1",
        "ap-southeast-4",
        "il-central-1",
        "us-gov-west-1",
    ] {
        let answer = emulator.get_endpoint(&[], region).await;
        assert_eq!(
            answer.status, 200,
            "case {region:?}: expected HTTP 200 from GetEndpoint, got {} {}",
            answer.status, answer.body
        );
        let endpoint = answer.string("endpoint");
        assert_device_advisor_host(region, &endpoint, region);
        assert_eq!(
            endpoint,
            account_level_host(region),
            "case {region:?}: the endpoint must be the host the shared derivation gives for \
             a request naming no device"
        );

        let again = emulator.get_endpoint(&[], region).await;
        assert_eq!(
            again.string("endpoint"),
            endpoint,
            "case {region:?}: the endpoint must be stable across calls"
        );
        for (other_region, other) in &seen {
            assert_ne!(
                &endpoint, other,
                "case {region:?}: a region of its own must be answered a host of its own, \
                 got {endpoint:?} for {other_region:?} too"
            );
        }
        seen.push((region, endpoint));
    }
}

/// A credential scope that is not a published region shape names no region this service
/// reflects anywhere, so the host carries the region its ARNs fall back to rather than the
/// caller-supplied value.
#[tokio::test]
async fn a_credential_scope_that_is_not_a_region_is_answered_the_default_regions_host() {
    let emulator = Emulator::new();

    // (case, the region in the credential scope)
    let rows: Vec<(&str, &str)> = vec![
        ("upper case", "US-EAST-1"),
        ("a host, not a region", "evil.example.com"),
        ("an ARN separator", "us-east-1:9999"),
        ("no trailing number", "us-east"),
        ("a single-letter prefix", "u-east-1"),
    ];

    for (case, region) in rows {
        let answer = emulator.get_endpoint(&[], region).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from GetEndpoint, got {} {}",
            answer.status, answer.body
        );
        let endpoint = answer.string("endpoint");
        assert_device_advisor_host(case, &endpoint, iotdeviceadvisor::state::DEFAULT_REGION);
        assert_eq!(
            endpoint,
            account_level_host(iotdeviceadvisor::state::DEFAULT_REGION),
            "case {case:?}: a scope that is not a published region must be answered the \
             default region's host"
        );
    }
}

/// One row of the cross-operation agreement table: a suite run's primary device, and the
/// `GetEndpoint` query that names that very device.
struct RunDeviceRow {
    /// What this row is called in an assertion message.
    case: &'static str,
    /// The region the run is started in and the endpoint is asked for.
    region: &'static str,
    /// `suiteRunConfiguration.primaryDevice`, as the start request carries it.
    primary_device: Value,
    /// The `GET /endpoint` query members naming the SAME device.
    query: Vec<(&'static str, String)>,
}

/// The host a suite run reports and PERSISTS for its primary device is the host
/// `GetEndpoint` vends for that same device: one derivation, so a client told where to
/// connect by `StartSuiteRun` is told the same place when it asks `GetEndpoint` — and it is
/// a host of that DEVICE's, which the account-level question is not answered.
///
/// The rows span the ways a run's primary device can be named (its thing, its certificate,
/// both, and a thing beside the role it assumes — `SuiteRunConfiguration$primaryDevice`
/// "requires a thing ARN or a certificate ARN") and the signing region.
#[tokio::test]
async fn a_run_reports_the_host_get_endpoint_vends_for_its_primary_device() {
    let emulator = Emulator::new();

    let rows: Vec<RunDeviceRow> = vec![
        RunDeviceRow {
            case: "thing only",
            region: "eu-west-3",
            primary_device: json!({ "thingArn": thing_arn("endpoint-device") }),
            query: vec![("thingArn", thing_arn("endpoint-device"))],
        },
        RunDeviceRow {
            case: "certificate only",
            region: "us-east-1",
            primary_device: json!({ "certificateArn": certificate_arn("run-cert") }),
            query: vec![("certificateArn", certificate_arn("run-cert"))],
        },
        RunDeviceRow {
            case: "thing and certificate",
            region: "ap-southeast-4",
            primary_device: json!({
                "thingArn": thing_arn("paired-device"),
                "certificateArn": certificate_arn("paired-cert"),
            }),
            query: vec![
                ("thingArn", thing_arn("paired-device")),
                ("certificateArn", certificate_arn("paired-cert")),
            ],
        },
        RunDeviceRow {
            case: "thing and the role it assumes",
            region: "us-gov-west-1",
            primary_device: json!({
                "thingArn": thing_arn("sigv4-device"),
                "deviceRoleArn": role_arn("DeviceRoleB"),
            }),
            query: vec![
                ("thingArn", thing_arn("sigv4-device")),
                ("deviceRoleArn", role_arn("DeviceRoleB")),
            ],
        },
    ];

    for RunDeviceRow {
        case,
        region,
        primary_device,
        query,
    } in rows
    {
        let created = emulator
            .call(
                "POST",
                "/suiteDefinitions",
                region,
                Some(json!({
                    "suiteDefinitionConfiguration": {
                        "suiteDefinitionName": format!("endpoint-suite-{case}"),
                        "rootGroup": "g",
                        "devicePermissionRoleArn": role_arn("DeviceAdvisorRole"),
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
                region,
                Some(json!({
                    "suiteRunConfiguration": { "primaryDevice": primary_device }
                })),
            )
            .await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected HTTP 200 from StartSuiteRun, got {} {}",
            started.status, started.body
        );
        let reported = started.string("endpoint");
        assert_device_advisor_host(case, &reported, region);

        let query: Vec<(&str, &str)> = query
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let vended = emulator
            .get_endpoint(&query, region)
            .await
            .string("endpoint");
        assert_eq!(
            reported, vended,
            "case {case:?}: StartSuiteRun must report the host GetEndpoint vends for the \
             run's primary device, got {reported:?} and {vended:?}"
        );

        let no_device = emulator.get_endpoint(&[], region).await.string("endpoint");
        assert_ne!(
            reported, no_device,
            "case {case:?}: a host is vended per device, so a run's host must not be the \
             host a request naming no device is answered, got {reported:?} for both"
        );

        let run_id = started.string("suiteRunId");
        assert_eq!(
            emulator.persisted_run_endpoint(&run_id).as_deref(),
            Some(reported.as_str()),
            "case {case:?}: the persisted run endpoint must be the host the start call \
             reported, got {:?}",
            emulator.persisted_run_endpoint(&run_id)
        );
    }
}

/// A suite run whose configuration names no device at all carries the host the shared
/// derivation gives for a configuration naming none: the same one `GetEndpoint` vends to a
/// request that names no device, since both ask the account-level question.
#[tokio::test]
async fn a_run_with_no_primary_device_carries_the_account_level_host() {
    let emulator = Emulator::new();
    let region = "us-west-2";

    // A run with an empty primary device cannot be STARTED over the wire — the member
    // "requires a thing ARN or a certificate ARN" — so the no-device case is taken
    // straight from the derivation every reporting operation shares.
    let derived = announced_host(region, &DeviceIdentity::default());
    let vended = emulator.get_endpoint(&[], region).await.string("endpoint");
    assert_eq!(
        derived, vended,
        "case \"no device\": the host derived for a run naming no device must be the host \
         GetEndpoint vends for a request naming none, got {derived:?} and {vended:?}"
    );
}

/// One row of the device-level table: a request that names a device, and the device it
/// names.
struct DeviceRow {
    /// What this row is called in an assertion message.
    case: &'static str,
    /// The query members the request carries.
    query: Vec<(&'static str, String)>,
    /// The DEVICE IDENTITY the row names — the ARNs it carries, and nothing else. Two rows
    /// naming one identity are answered one host, and two identities two: the
    /// authentication method says how the device connects rather than which device it is, so
    /// it is no part of the identity.
    identity: &'static str,
}

/// The device identities the device-level rule is asserted over: rows differing in WHICH
/// of the three members name the device, in HOW MANY of them do, and in the
/// `AuthenticationMethod` the request carries (both values the model enumerates).
fn device_rows() -> Vec<DeviceRow> {
    let row = |case, query, identity| DeviceRow {
        case,
        query,
        identity,
    };
    vec![
        row(
            "thing only",
            vec![("thingArn", thing_arn("device-a"))],
            "thing-a",
        ),
        row(
            "the same thing with X.509",
            vec![
                ("thingArn", thing_arn("device-a")),
                ("authenticationMethod", "X509ClientCertificate".to_string()),
            ],
            "thing-a",
        ),
        row(
            "another thing",
            vec![("thingArn", thing_arn("device-b"))],
            "thing-b",
        ),
        row(
            "certificate only",
            vec![("certificateArn", certificate_arn("cert-a"))],
            "cert-a",
        ),
        row(
            "role with SigV4",
            vec![
                ("deviceRoleArn", role_arn("DeviceRoleA")),
                ("authenticationMethod", "SignatureVersion4".to_string()),
            ],
            "role-a",
        ),
        row(
            "the same role with no method named",
            vec![("deviceRoleArn", role_arn("DeviceRoleA"))],
            "role-a",
        ),
        row(
            "thing and certificate",
            vec![
                ("thingArn", thing_arn("device-a")),
                ("certificateArn", certificate_arn("cert-a")),
            ],
            "thing-a+cert-a",
        ),
        row(
            "every member, SigV4",
            vec![
                ("thingArn", thing_arn("device-a")),
                ("certificateArn", certificate_arn("cert-a")),
                ("deviceRoleArn", role_arn("DeviceRoleA")),
                ("authenticationMethod", "SignatureVersion4".to_string()),
            ],
            "thing-a+cert-a+role-a",
        ),
    ]
}

/// A request that names a device is answered a Device Advisor host of THAT device: one
/// identity is answered one host however it is written and however many calls ask for it,
/// two identities are answered two, and none of them is the host the account-level question
/// gets — while the authentication method, which says how the device connects rather than
/// which device it is, moves nothing.
#[tokio::test]
async fn a_named_device_is_answered_a_host_of_its_own() {
    let emulator = Emulator::new();
    let region = "us-east-1";
    let no_device = emulator.get_endpoint(&[], region).await.string("endpoint");

    // Every row already asserted, so a later row can be compared with it.
    let mut seen: Vec<(&str, &str, String)> = Vec::new();
    for DeviceRow {
        case,
        query: members,
        identity,
    } in device_rows()
    {
        let query: Vec<(&str, &str)> = members
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let answer = emulator.get_endpoint(&query, region).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from GetEndpoint, got {} {}",
            answer.status, answer.body
        );
        let endpoint = answer.string("endpoint");
        assert_device_advisor_host(case, &endpoint, region);
        assert_ne!(
            endpoint, no_device,
            "case {case:?}: naming a device must be answered a host of that device, not the \
             one a request naming none is answered ({no_device:?})"
        );

        let again = emulator.get_endpoint(&query, region).await;
        assert_eq!(
            again.string("endpoint"),
            endpoint,
            "case {case:?}: the same request must get the same endpoint on every call"
        );

        for (other_case, other_identity, other) in &seen {
            if *other_identity == identity {
                assert_eq!(
                    &endpoint, other,
                    "case {case:?}: names the device case {other_case:?} does, so it must be \
                     answered that host, got {endpoint:?}"
                );
            } else {
                assert_ne!(
                    &endpoint, other,
                    "case {case:?}: names another device than case {other_case:?}, so it \
                     must be answered another host, got {endpoint:?} for both"
                );
            }
        }
        seen.push((case, identity, endpoint));
    }
}

/// The host reported names the REGION the request is signed for: one device asked about in
/// three regions is answered three hosts, each naming its own region, which is what a
/// client reads off the value to know where that device connects.
#[tokio::test]
async fn a_device_host_names_the_requests_region() {
    let emulator = Emulator::new();
    let thing = thing_arn("multi-region-device");
    let query = [("thingArn", thing.as_str())];

    let mut seen: Vec<(&str, String)> = Vec::new();
    for region in ["us-east-1", "eu-central-1", "ap-southeast-4"] {
        let endpoint = emulator
            .get_endpoint(&query, region)
            .await
            .string("endpoint");
        assert_device_advisor_host(region, &endpoint, region);
        assert_eq!(
            endpoint,
            announced_host(
                region,
                &DeviceIdentity {
                    thing_arn: Some(&thing),
                    ..DeviceIdentity::default()
                }
            ),
            "case {region:?}: the host must be the one the shared derivation gives for this \
             device in this region"
        );
        for (other_region, other) in &seen {
            assert_ne!(
                &endpoint, other,
                "case {region:?}: a device's host in {region} must not be the host it is \
                 answered in {other_region}"
            );
        }
        seen.push((region, endpoint));
    }
}

/// An ARN that is not one for the member carrying it is refused with the operation's
/// modelled client-input error — never a 501 and never a host.
#[tokio::test]
async fn a_malformed_request_is_refused() {
    let emulator = Emulator::new();

    // (case, query) — every ARN here is at least the modelled 20 characters, so what is
    // under test is the ARN form, not the length bound.
    let rows: Vec<(&str, Vec<(&str, String)>)> = vec![
        (
            "thingArn is not an ARN at all",
            vec![("thingArn", "my-favourite-thing-name".to_string())],
        ),
        (
            "thingArn is an IAM role ARN",
            vec![("thingArn", role_arn("NotAThing"))],
        ),
        (
            "certificateArn is an IAM role ARN",
            vec![("certificateArn", role_arn("NotACertificate"))],
        ),
        (
            "deviceRoleArn is an IoT thing ARN",
            vec![("deviceRoleArn", thing_arn("not-a-role"))],
        ),
        (
            "thingArn has no resource",
            vec![("thingArn", "arn:aws:iot:us-east-1:000000000000:".to_string())],
        ),
        (
            "thingArn has no partition",
            vec![("thingArn", "arn::iot:us-east-1:000000000000:thing/x".to_string())],
        ),
        (
            "thingArn has too few fields",
            vec![("thingArn", "arn:aws:iot:thing/my-thing-name".to_string())],
        ),
        (
            "thingArn is an IAM role ARN, beside an authentication method",
            vec![
                ("thingArn", role_arn("NotAThing")),
                ("authenticationMethod", "SignatureVersion4".to_string()),
            ],
        ),
    ];

    for (case, members) in rows {
        let query: Vec<(&str, &str)> = members
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let answer = emulator.get_endpoint(&query, "us-east-1").await;
        assert_eq!(
            (answer.status, answer.error_code.as_deref()),
            (400, Some("ValidationException")),
            "case {case:?}: expected a 400 ValidationException, got {} {:?} {}",
            answer.status,
            answer.error_code,
            answer.body
        );
    }
}

/// The modelled constraints are refused before the handler runs: an ARN below the member's
/// minimum length and an `authenticationMethod` outside the enum are both 400s, not hosts.
#[tokio::test]
async fn values_outside_the_modelled_constraints_are_refused() {
    let emulator = Emulator::new();

    // (case, query) — `AmazonResourceName` is len 20..2048 and `AuthenticationMethod` is
    // an enum of exactly two values (source: `smithy_query(query="operation",
    // operation="GetEndpoint")`).
    let rows: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("thingArn below the minimum length", vec![("thingArn", "arn:aws:iot:::t")]),
        (
            "certificateArn below the minimum length",
            vec![("certificateArn", "arn:aws:iot:::c")],
        ),
        (
            "authenticationMethod outside the enum",
            vec![("authenticationMethod", "Anonymous")],
        ),
    ];

    for (case, query) in rows {
        let answer = emulator.get_endpoint(&query, "us-east-1").await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: a value outside a modelled constraint must be refused, got {} {}",
            answer.status, answer.body
        );
    }
}

/// An authentication method on its own names no device, so it is answered what a request
/// naming no device is answered — for both values the model enumerates.
#[tokio::test]
async fn an_authentication_method_alone_still_asks_for_the_account_endpoint() {
    let emulator = Emulator::new();
    let region = "us-east-1";
    let no_device = emulator.get_endpoint(&[], region).await.string("endpoint");

    for method in ["X509ClientCertificate", "SignatureVersion4"] {
        let answer = emulator
            .get_endpoint(&[("authenticationMethod", method)], region)
            .await;
        assert_eq!(
            answer.status, 200,
            "case {method:?}: expected HTTP 200 from GetEndpoint, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.string("endpoint"),
            no_device,
            "case {method:?}: a request naming no device must get the endpoint a request \
             naming none is answered"
        );
    }
}

/// `authenticationMethod` is an OPTIONAL member the model couples to no other member, so
/// adding it to a request that is otherwise answered cannot change the answer: not its
/// status and not the host. Asserted over every way a request can name its device — none
/// of the three members, each one alone, each pair and all three — crossed with the method
/// absent and with each of the two values the model enumerates (source:
/// `smithy_query(query="operation",operation="GetEndpoint")`, where every identity member
/// is optional and `authenticationMethod` targets the two-value `AuthenticationMethod`
/// enum). Every row's three answers must be the one host that row's identity is answered.
#[tokio::test]
async fn an_optional_authentication_method_never_changes_the_answer() {
    let emulator = Emulator::new();
    let region = "us-east-1";

    let thing = thing_arn("independent-device");
    let certificate = certificate_arn("independent-cert");
    let role = role_arn("IndependentRole");

    // (case, the identity members the request carries) — the full subset lattice of the
    // three optional identity members.
    let identities: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("no device", vec![]),
        ("thing only", vec![("thingArn", thing.as_str())]),
        (
            "certificate only",
            vec![("certificateArn", certificate.as_str())],
        ),
        ("device role only", vec![("deviceRoleArn", role.as_str())]),
        (
            "thing and certificate",
            vec![
                ("thingArn", thing.as_str()),
                ("certificateArn", certificate.as_str()),
            ],
        ),
        (
            "thing and device role",
            vec![
                ("thingArn", thing.as_str()),
                ("deviceRoleArn", role.as_str()),
            ],
        ),
        (
            "certificate and device role",
            vec![
                ("certificateArn", certificate.as_str()),
                ("deviceRoleArn", role.as_str()),
            ],
        ),
        (
            "every identity member",
            vec![
                ("thingArn", thing.as_str()),
                ("certificateArn", certificate.as_str()),
                ("deviceRoleArn", role.as_str()),
            ],
        ),
    ];

    for (identity_case, identity) in identities {
        let baseline = emulator.get_endpoint(&identity, region).await;
        assert_eq!(
            baseline.status, 200,
            "case {identity_case:?}: an identity of optional members alone is answered, got \
             {} {}",
            baseline.status, baseline.body
        );
        let expected = baseline.string("endpoint");
        assert_device_advisor_host(identity_case, &expected, region);

        for method in ["SignatureVersion4", "X509ClientCertificate"] {
            let case = format!("{identity_case} with {method}");
            let mut query = identity.clone();
            query.push(("authenticationMethod", method));
            let answer = emulator.get_endpoint(&query, region).await;
            assert_eq!(
                (answer.status, answer.error_code.as_deref()),
                (200, None),
                "case {case:?}: adding the optional authenticationMethod must not refuse a \
                 request the same identity is answered for, got {} {:?} {}",
                answer.status,
                answer.error_code,
                answer.body
            );
            assert_eq!(
                answer.string("endpoint"),
                expected,
                "case {case:?}: the method is no part of the device's identity, so the host \
                 must equal the one for case {identity_case:?} ({expected:?}), got {:?}",
                answer.string("endpoint")
            );
        }
    }
}
