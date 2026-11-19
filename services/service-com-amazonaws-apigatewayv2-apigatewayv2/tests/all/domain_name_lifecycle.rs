// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the ApiGatewayV2 custom `DomainName` lifecycle:
//! `CreateDomainName`, `GetDomainName` and `UpdateDomainName`.
//!
//! Every test drives the real `restJson1` request path — `MultiServiceRouter::dispatch`
//! over a signed `http::Request` — so what is asserted is the wire behaviour a client
//! sees, not a handler called directly.
//!
//! The rules asserted here, each over a TABLE rather than over one request:
//!
//! * A custom domain name is a persisted resource keyed by its own name: what a create
//!   reports is what a later read of the same name reports, and an operation addressing
//!   a name the account does not hold is refused with the modelled `NotFoundException`
//!   (404) — which is the recorded case that filed the issue, one row of its table.
//! * `PATCH /v2/domainnames/{DomainName}` is a patch of the STORED record: a member the
//!   request carries replaces the stored one, a member it omits leaves the stored value
//!   as it was, and the identifiers the resource was minted with (`DomainNameArn`, the
//!   `ApiGatewayDomainName` host) never move.
//! * A custom domain name is served by the endpoints its `DomainNameConfigurations`
//!   describe, so a request that would leave it with no entry is refused with the
//!   modelled `BadRequestException` (400) — the table in
//!   `domain_name_endpoint_required` asserts that rule over both writers.
//! * `DomainNameStatus` is `UPDATING` while the endpoint migration an update started is
//!   still due and `AVAILABLE` once it is not, and "if the status is UPDATING, the
//!   domain cannot be modified further until the existing operation is complete"
//!   (`DomainNameConfiguration$DomainNameStatus`).
//! * **A custom domain name's ARN names it by its API Gateway management path with an EMPTY
//!   account-id field** — `arn:aws:apigateway:<region>::/domainnames/<domainName>`, with
//!   the Region taken from the request's credential scope — and it is built once, when the
//!   resource is created, so every later reader reports the same ARN whatever Region that
//!   reader signs for. An ARN carrying an account id never equals the one AWS produces for
//!   the same domain name, so no IAM resource pattern or cross-service reference a caller
//!   builds would match it.
//! * **`CreateDomainName` answers the tags its own request attached**, and a request that
//!   attached none answers no `tags` member at all — the member's presence is what a client
//!   tests to tell a tagged resource from an untagged one — **while every READER of the
//!   resource reports its tag collection**, empty or not.
//!
//! The rows vary what each rule generalises over: how many configurations a request
//! carries (one, several), which members of a configuration it sets, which members
//! of the patch it names (none, one, several, all), every value of the modelled
//! `RoutingMode` enum, the published bounds of the domain name's length, the form of
//! an absent name, and — for the last two rules — the Region the request is signed for, the
//! form of the domain name, and whether the create attached tags.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_apigatewayv2_apigatewayv2::ops::types::{
    DOMAIN_NAME_STATUS_VALUES, ROUTING_MODE_VALUES,
};
use service_com_amazonaws_apigatewayv2_apigatewayv2::state::duplicate_identity::DOMAIN_NAME_ALREADY_EXISTS_MESSAGE;
use service_com_amazonaws_apigatewayv2_apigatewayv2::state::{
    API_MAPPING_SELECTION_EXPRESSION, DEFAULT_ENDPOINT_TYPE, DEFAULT_IP_ADDRESS_TYPE,
    DEFAULT_ROUTING_MODE, DEFAULT_SECURITY_POLICY, DOMAIN_NAME_MAX_LENGTH, DOMAIN_NAME_MIGRATION,
    DOMAIN_NAME_STATUS_AVAILABLE, DOMAIN_NAME_STATUS_UPDATING,
};

use tokio::time::sleep;

use crate::api_lifecycle::{as_str, call_in_region, emulator, error_code};

/// The region every request in this suite is signed for. Deliberately NOT the
/// emulator's `us-east-1` default, so an ARN or a host built from a hardcoded region
/// fails.
const REGION: &str = "eu-central-1";

/// How long a poll for the endpoint migration to come due may run before the test gives
/// up, and how long it waits between looks. Both are generous next to
/// `DOMAIN_NAME_MIGRATION`, the span the service owes the migration: the deadline is the
/// only bound, and it is far above anything a loaded host needs.
const MIGRATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);
const MIGRATION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// An ACM certificate ARN, the only source `DomainNameConfiguration$CertificateArn`
/// documents ("AWS Certificate Manager is the only supported source"). Built through the
/// emulator's own ARN helper rather than written out, so the account segment is the
/// canonical one.
pub(crate) fn certificate_arn(name: &str) -> String {
    engine::emulator_arn("acm", REGION, &format!("certificate/{name}"))
}

/// The ARN a custom domain name named `domain`, created in `region`, must report: the API
/// Gateway management form, whose account-id field is EMPTY. Written out here rather than
/// built through the emulator's ARN helper, which always writes the account in.
pub(crate) fn expected_domain_name_arn(region: &str, domain: &str) -> String {
    format!("arn:aws:apigateway:{region}::/domainnames/{domain}")
}

/// Send one request to `/v2/domainnames…` and answer `(status, body)`.
pub(crate) async fn call(
    emulator: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call_in_region(emulator, REGION, method, uri, body).await
}

pub(crate) async fn create_domain_name(
    emulator: &MultiServiceRouter,
    request: &Value,
) -> (u16, Value) {
    call(emulator, "POST", "/v2/domainnames", Some(request.clone())).await
}

pub(crate) async fn get_domain_name(emulator: &MultiServiceRouter, domain: &str) -> (u16, Value) {
    call(emulator, "GET", &format!("/v2/domainnames/{domain}"), None).await
}

pub(crate) async fn update_domain_name(
    emulator: &MultiServiceRouter,
    domain: &str,
    patch: &Value,
) -> (u16, Value) {
    call(
        emulator,
        "PATCH",
        &format!("/v2/domainnames/{domain}"),
        Some(patch.clone()),
    )
    .await
}

/// Create one domain name, asserting the 201 the model binds to `CreateDomainName`, and
/// answer the created body.
async fn created(emulator: &MultiServiceRouter, case: &str, request: &Value) -> Value {
    let (status, body) = create_domain_name(emulator, request).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateDomainName, got {status} {body}"
    );
    body
}

/// The `domainNameConfigurations` of a response, as a list.
pub(crate) fn configurations(body: &Value) -> Vec<Value> {
    body.get("domainNameConfigurations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// One member of one configuration entry, as a string (`""` when absent).
fn configuration_member(body: &Value, index: usize, member: &str) -> String {
    configurations(body)
        .get(index)
        .map(|entry| as_str(entry, member))
        .unwrap_or_default()
}

/// One row of the `CreateDomainName` table.
struct CreateCase {
    label: &'static str,
    domain: &'static str,
    /// The request body, beyond `domainName`.
    extra: Value,
    expect_routing_mode: String,
    /// For each configuration the response must carry, in order:
    /// `(endpointType, securityPolicy, ipAddressType, certificateArn)`.
    expect_configurations: Vec<(String, String, String, String)>,
    /// What the CREATE's own answer must carry under `tags`: the tags the request
    /// attached, and `null` — no member at all — for a request that attached none. A
    /// READER of the resource reports its collection either way, which
    /// [`the_create_answers_the_tags_it_attached_and_every_reader_the_collection`] owns.
    expect_tags: Value,
}

fn create_cases() -> Vec<CreateCase> {
    vec![
        CreateCase {
            label: "one configuration that names nothing itself",
            domain: "bare.example.com",
            extra: json!({"domainNameConfigurations": [{}]}),
            expect_routing_mode: DEFAULT_ROUTING_MODE.to_string(),
            expect_configurations: vec![(
                DEFAULT_ENDPOINT_TYPE.to_string(),
                DEFAULT_SECURITY_POLICY.to_string(),
                DEFAULT_IP_ADDRESS_TYPE.to_string(),
                String::new(),
            )],
            expect_tags: json!(null),
        },
        CreateCase {
            label: "one configuration naming only a certificate",
            domain: "one-config.example.com",
            extra: json!({
                "domainNameConfigurations": [
                    {"certificateArn": certificate_arn("one-config")}
                ]
            }),
            expect_routing_mode: DEFAULT_ROUTING_MODE.to_string(),
            expect_configurations: vec![(
                DEFAULT_ENDPOINT_TYPE.to_string(),
                DEFAULT_SECURITY_POLICY.to_string(),
                DEFAULT_IP_ADDRESS_TYPE.to_string(),
                certificate_arn("one-config"),
            )],
            expect_tags: json!(null),
        },
        CreateCase {
            label: "one configuration setting every defaulted enum itself",
            domain: "explicit.example.com",
            extra: json!({
                "domainNameConfigurations": [{
                    "certificateArn": certificate_arn("explicit"),
                    "certificateName": "explicit-cert",
                    "endpointType": "EDGE",
                    "securityPolicy": "TLS_1_2",
                    "ipAddressType": "dualstack"
                }],
                "routingMode": "ROUTING_RULE_ONLY"
            }),
            expect_routing_mode: "ROUTING_RULE_ONLY".to_string(),
            expect_configurations: vec![(
                "EDGE".to_string(),
                "TLS_1_2".to_string(),
                "dualstack".to_string(),
                certificate_arn("explicit"),
            )],
            expect_tags: json!(null),
        },
        CreateCase {
            label: "three configurations, tags, and mutual TLS",
            domain: "many.example.com",
            extra: json!({
                "domainNameConfigurations": [
                    {"certificateArn": certificate_arn("first"), "ipAddressType": "dualstack"},
                    {"certificateArn": certificate_arn("second"), "securityPolicy": "TLS_1_2"},
                    {"certificateArn": certificate_arn("third"), "endpointType": "EDGE"}
                ],
                "mutualTlsAuthentication": {
                    "truststoreUri": "s3://truststores/many.pem",
                    "truststoreVersion": "v1"
                },
                "routingMode": "ROUTING_RULE_THEN_API_MAPPING",
                "tags": {"team": "edge", "env": "test"}
            }),
            expect_routing_mode: "ROUTING_RULE_THEN_API_MAPPING".to_string(),
            expect_configurations: vec![
                (
                    DEFAULT_ENDPOINT_TYPE.to_string(),
                    DEFAULT_SECURITY_POLICY.to_string(),
                    "dualstack".to_string(),
                    certificate_arn("first"),
                ),
                (
                    DEFAULT_ENDPOINT_TYPE.to_string(),
                    "TLS_1_2".to_string(),
                    DEFAULT_IP_ADDRESS_TYPE.to_string(),
                    certificate_arn("second"),
                ),
                (
                    "EDGE".to_string(),
                    DEFAULT_SECURITY_POLICY.to_string(),
                    DEFAULT_IP_ADDRESS_TYPE.to_string(),
                    certificate_arn("third"),
                ),
            ],
            expect_tags: json!({"team": "edge", "env": "test"}),
        },
        CreateCase {
            label: "the shortest name the target shape allows",
            domain: "a",
            extra: json!({
                "domainNameConfigurations": [{"certificateArn": certificate_arn("shortest")}]
            }),
            expect_routing_mode: DEFAULT_ROUTING_MODE.to_string(),
            expect_configurations: vec![(
                DEFAULT_ENDPOINT_TYPE.to_string(),
                DEFAULT_SECURITY_POLICY.to_string(),
                DEFAULT_IP_ADDRESS_TYPE.to_string(),
                certificate_arn("shortest"),
            )],
            expect_tags: json!(null),
        },
    ]
}

#[tokio::test]
async fn a_created_domain_name_is_reported_back_by_get_domain_name() {
    let emulator = emulator();

    for case in create_cases() {
        let label = case.label;
        let mut request = json!({"domainName": case.domain});
        for (member, value) in case.extra.as_object().expect("the extra members") {
            request[member] = value.clone();
        }

        let body = created(&emulator, label, &request).await;

        assert_eq!(
            as_str(&body, "domainName"),
            case.domain,
            "case {label:?}: expected the created domain name to be reported, got {body}"
        );
        assert_eq!(
            as_str(&body, "domainNameArn"),
            expected_domain_name_arn(REGION, case.domain),
            "case {label:?}: expected the ARN of the created resource, got {body}"
        );
        assert_eq!(
            as_str(&body, "routingMode"),
            case.expect_routing_mode,
            "case {label:?}: expected routingMode {:?}, got {body}",
            case.expect_routing_mode
        );
        assert_eq!(
            as_str(&body, "apiMappingSelectionExpression"),
            API_MAPPING_SELECTION_EXPRESSION,
            "case {label:?}: expected the api mapping selection expression, got {body}"
        );
        assert_eq!(
            body.get("tags").cloned().unwrap_or(Value::Null),
            case.expect_tags,
            "case {label:?}: expected tags {}, got {body}",
            case.expect_tags
        );

        let entries = configurations(&body);
        assert_eq!(
            entries.len(),
            case.expect_configurations.len(),
            "case {label:?}: expected {} configuration(s), got {body}",
            case.expect_configurations.len()
        );
        for (index, (endpoint_type, security_policy, ip_address_type, cert)) in
            case.expect_configurations.iter().enumerate()
        {
            let entry = &entries[index];
            assert_eq!(
                &as_str(entry, "endpointType"),
                endpoint_type,
                "case {label:?}: configuration {index} expected endpointType {endpoint_type:?}, \
                 got {entry}"
            );
            assert_eq!(
                &as_str(entry, "securityPolicy"),
                security_policy,
                "case {label:?}: configuration {index} expected securityPolicy \
                 {security_policy:?}, got {entry}"
            );
            assert_eq!(
                &as_str(entry, "ipAddressType"),
                ip_address_type,
                "case {label:?}: configuration {index} expected ipAddressType \
                 {ip_address_type:?}, got {entry}"
            );
            assert_eq!(
                &as_str(entry, "certificateArn"),
                cert,
                "case {label:?}: configuration {index} expected certificateArn {cert:?}, \
                 got {entry}"
            );
            // A domain name AWS has nothing left to migrate is `AVAILABLE`, the status
            // under which the model says "the domain can be updated".
            assert_eq!(
                as_str(entry, "domainNameStatus"),
                DOMAIN_NAME_STATUS_AVAILABLE,
                "case {label:?}: configuration {index} expected a created domain name to be \
                 {DOMAIN_NAME_STATUS_AVAILABLE}, got {entry}"
            );
            // Every configuration of one domain name is served on the one host that
            // domain name was minted with.
            assert_eq!(
                as_str(entry, "apiGatewayDomainName"),
                configuration_member(&body, 0, "apiGatewayDomainName"),
                "case {label:?}: configuration {index} expected the domain name's one \
                 apiGatewayDomainName, got {entry}"
            );
        }

        // The round trip: a read of the same name reports exactly what the create did,
        // except under `tags` — the create answers the tags its own request attached while
        // a read reports the resource's collection, empty or not (see
        // [`the_create_answers_the_tags_it_attached_and_every_reader_the_collection`]).
        let (status, read) = get_domain_name(&emulator, case.domain).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected 200 from GetDomainName, got {status} {read}"
        );
        let mut expected_read = body.clone();
        expected_read["tags"] = case
            .expect_tags
            .as_object()
            .cloned()
            .map_or_else(|| json!({}), Value::Object);
        assert_eq!(
            read, expected_read,
            "case {label:?}: expected GetDomainName to report the created record, got {read}"
        );
    }
}

#[tokio::test]
async fn each_domain_name_publishes_an_address_of_its_own_a_client_can_dial() {
    let emulator = emulator();
    // `apiGatewayDomainName` is "the domain name associated with the regional endpoint for
    // this custom domain name" — the value a caller points its own DNS record at and a client
    // then DIALS — so what is published is the address this emulator serves that domain name
    // at (`crate::service::domain_name_socket`), not a name under API Gateway's own zone that
    // resolves nowhere on this machine. Two domain names of one account are served on
    // DIFFERENT addresses, so a request reaches the one it was addressed to.
    let mut hosts = Vec::new();
    for domain in ["host-a.example.com", "host-b.example.com"] {
        let body = created(&emulator, domain, &json!({
            "domainName": domain,
            "domainNameConfigurations": [{"certificateArn": certificate_arn("host")}]
        }))
        .await;
        let host = configuration_member(&body, 0, "apiGatewayDomainName");
        assert!(
            host.parse::<std::net::SocketAddr>().is_ok(),
            "case {domain:?}: expected an address a client can dial, got {host:?}"
        );
        hosts.push(host);
    }
    assert_ne!(
        hosts[0], hosts[1],
        "expected two domain names to be served on distinct hosts, got {hosts:?}"
    );
}

/// One row of the `UpdateDomainName` patch table.
struct PatchCase {
    label: &'static str,
    patch: Value,
    /// The `routingMode` the patched record must carry.
    expect_routing_mode: String,
    /// The `truststoreUri` the patched record must carry (`""` for none).
    expect_truststore_uri: &'static str,
    /// The `certificateArn` of each configuration the patched record must carry.
    expect_certificates: Vec<String>,
    /// Whether the patch starts an endpoint migration, which is what moves the status
    /// off `AVAILABLE`.
    expect_status: &'static str,
}

/// The record every patch row starts from: one configuration, mutual TLS, tags, and a
/// routing mode that is not the default.
fn patch_seed(domain: &str) -> Value {
    json!({
        "domainName": domain,
        "domainNameConfigurations": [{"certificateArn": certificate_arn("seed")}],
        "mutualTlsAuthentication": {
            "truststoreUri": "s3://truststores/seed.pem",
            "truststoreVersion": "v1"
        },
        "routingMode": "ROUTING_RULE_ONLY",
        "tags": {"owner": "seed"}
    })
}

fn patch_cases() -> Vec<PatchCase> {
    vec![
        PatchCase {
            label: "an empty patch changes nothing",
            patch: json!({}),
            expect_routing_mode: "ROUTING_RULE_ONLY".to_string(),
            expect_truststore_uri: "s3://truststores/seed.pem",
            expect_certificates: vec![certificate_arn("seed")],
            expect_status: DOMAIN_NAME_STATUS_AVAILABLE,
        },
        PatchCase {
            label: "routingMode alone",
            patch: json!({"routingMode": "API_MAPPING_ONLY"}),
            expect_routing_mode: "API_MAPPING_ONLY".to_string(),
            expect_truststore_uri: "s3://truststores/seed.pem",
            expect_certificates: vec![certificate_arn("seed")],
            expect_status: DOMAIN_NAME_STATUS_AVAILABLE,
        },
        PatchCase {
            label: "mutualTlsAuthentication alone",
            patch: json!({
                "mutualTlsAuthentication": {
                    "truststoreUri": "s3://truststores/patched.pem",
                    "truststoreVersion": "v7"
                }
            }),
            expect_routing_mode: "ROUTING_RULE_ONLY".to_string(),
            expect_truststore_uri: "s3://truststores/patched.pem",
            expect_certificates: vec![certificate_arn("seed")],
            expect_status: DOMAIN_NAME_STATUS_AVAILABLE,
        },
        PatchCase {
            label: "re-sending the stored configuration asks for no migration",
            patch: json!({
                "domainNameConfigurations": [{"certificateArn": certificate_arn("seed")}]
            }),
            expect_routing_mode: "ROUTING_RULE_ONLY".to_string(),
            expect_truststore_uri: "s3://truststores/seed.pem",
            expect_certificates: vec![certificate_arn("seed")],
            expect_status: DOMAIN_NAME_STATUS_AVAILABLE,
        },
        PatchCase {
            label: "a different certificate migrates the endpoint",
            patch: json!({
                "domainNameConfigurations": [{"certificateArn": certificate_arn("rotated")}]
            }),
            expect_routing_mode: "ROUTING_RULE_ONLY".to_string(),
            expect_truststore_uri: "s3://truststores/seed.pem",
            expect_certificates: vec![certificate_arn("rotated")],
            expect_status: DOMAIN_NAME_STATUS_UPDATING,
        },
        PatchCase {
            label: "a longer configuration list replaces the stored one wholesale",
            patch: json!({
                "domainNameConfigurations": [
                    {"certificateArn": certificate_arn("left")},
                    {"certificateArn": certificate_arn("right"), "endpointType": "EDGE"}
                ]
            }),
            expect_routing_mode: "ROUTING_RULE_ONLY".to_string(),
            expect_truststore_uri: "s3://truststores/seed.pem",
            expect_certificates: vec![certificate_arn("left"), certificate_arn("right")],
            expect_status: DOMAIN_NAME_STATUS_UPDATING,
        },
        PatchCase {
            label: "every patchable member at once",
            patch: json!({
                "domainNameConfigurations": [{
                    "certificateArn": certificate_arn("all"),
                    "securityPolicy": "TLS_1_2"
                }],
                "mutualTlsAuthentication": {"truststoreUri": "s3://truststores/all.pem"},
                "routingMode": "ROUTING_RULE_THEN_API_MAPPING"
            }),
            expect_routing_mode: "ROUTING_RULE_THEN_API_MAPPING".to_string(),
            expect_truststore_uri: "s3://truststores/all.pem",
            expect_certificates: vec![certificate_arn("all")],
            expect_status: DOMAIN_NAME_STATUS_UPDATING,
        },
    ]
}

#[tokio::test]
async fn update_domain_name_patches_the_stored_record() {
    for (row, case) in patch_cases().into_iter().enumerate() {
        // One emulator per row, so a row's patch is applied to a freshly seeded record.
        let emulator = emulator();
        let label = case.label;
        let domain = format!("patch-{row}.example.com");
        let seed = created(&emulator, label, &patch_seed(&domain)).await;

        let (status, patched) = update_domain_name(&emulator, &domain, &case.patch).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected 200 from UpdateDomainName, got {status} {patched}"
        );

        assert_eq!(
            as_str(&patched, "routingMode"),
            case.expect_routing_mode,
            "case {label:?}: expected routingMode {:?}, got {patched}",
            case.expect_routing_mode
        );
        assert_eq!(
            patched
                .get("mutualTlsAuthentication")
                .map(|mtls| as_str(mtls, "truststoreUri"))
                .unwrap_or_default(),
            case.expect_truststore_uri,
            "case {label:?}: expected truststoreUri {:?}, got {patched}",
            case.expect_truststore_uri
        );
        let certificates: Vec<String> = configurations(&patched)
            .iter()
            .map(|entry| as_str(entry, "certificateArn"))
            .collect();
        assert_eq!(
            certificates, case.expect_certificates,
            "case {label:?}: expected configurations {:?}, got {patched}",
            case.expect_certificates
        );
        for (index, entry) in configurations(&patched).iter().enumerate() {
            assert_eq!(
                as_str(entry, "domainNameStatus"),
                case.expect_status,
                "case {label:?}: configuration {index} expected status {:?}, got {entry}",
                case.expect_status
            );
        }

        // Neither the resource's ARN nor the host a caller's DNS record points at moves
        // when its configuration does, and a member no patch names keeps its value —
        // `Tags` is not even an input member of `UpdateDomainName`.
        assert_eq!(
            as_str(&patched, "domainNameArn"),
            as_str(&seed, "domainNameArn"),
            "case {label:?}: expected the ARN to be unchanged, got {patched}"
        );
        assert_eq!(
            as_str(&patched, "domainName"),
            domain,
            "case {label:?}: expected the patched record to keep its name, got {patched}"
        );
        assert_eq!(
            patched.get("tags").cloned().unwrap_or(Value::Null),
            json!({"owner": "seed"}),
            "case {label:?}: expected the seeded tags to survive the patch, got {patched}"
        );
        if !configurations(&patched).is_empty() {
            assert_eq!(
                configuration_member(&patched, 0, "apiGatewayDomainName"),
                configuration_member(&seed, 0, "apiGatewayDomainName"),
                "case {label:?}: expected the minted host to be unchanged, got {patched}"
            );
        }

        // What the patch reported is what a later read reports.
        let (status, read) = get_domain_name(&emulator, &domain).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected 200 from GetDomainName, got {status} {read}"
        );
        assert_eq!(
            read, patched,
            "case {label:?}: expected GetDomainName to report the patched record, got {read}"
        );
    }
}

#[tokio::test]
async fn every_modelled_routing_mode_round_trips() {
    let emulator = emulator();
    // The catalog is the model's own (`smithy_query(query="shape", shape="RoutingMode")`),
    // reached through the generated constant rather than written out here, so a value the
    // model gains later fails this test instead of going untested.
    for mode in ROUTING_MODE_VALUES {
        let domain = format!("{}.example.com", mode.to_ascii_lowercase().replace('_', "-"));
        let created_body = created(
            &emulator,
            mode,
            &json!({
                "domainName": domain,
                "domainNameConfigurations": [{"certificateArn": certificate_arn(mode)}],
                "routingMode": mode
            }),
        )
        .await;
        assert_eq!(
            &as_str(&created_body, "routingMode"),
            mode,
            "case {mode:?}: expected the created routingMode to be reported, got {created_body}"
        );

        // Every mode is also reachable by patching a domain name created without one.
        let patched_domain = format!("patched-{domain}");
        created(
            &emulator,
            mode,
            &json!({
                "domainName": patched_domain,
                "domainNameConfigurations": [{"certificateArn": certificate_arn(mode)}]
            }),
        )
        .await;
        let (status, patched) =
            update_domain_name(&emulator, &patched_domain, &json!({"routingMode": mode})).await;
        assert_eq!(
            status, 200,
            "case {mode:?}: expected 200 from UpdateDomainName, got {status} {patched}"
        );
        assert_eq!(
            &as_str(&patched, "routingMode"),
            mode,
            "case {mode:?}: expected the patched routingMode to be reported, got {patched}"
        );
    }
}

#[tokio::test]
async fn an_absent_domain_name_is_not_found() {
    let emulator = emulator();
    // One domain name the account DOES hold, so what the rows below exercise is the
    // lookup and not an empty store.
    created(
        &emulator,
        "neighbour",
        &json!({
            "domainName": "held.example.com",
            "domainNameConfigurations": [{"certificateArn": certificate_arn("held")}]
        }),
    )
    .await;

    // The rows vary the form of the absent name: a plain one, a sibling of the held
    // name, one differing only in case, and a deep label. The recorded unit that filed
    // the issue — an `UpdateDomainName` against a name that was never created — is the
    // first row.
    for absent in [
        "never-created.example.com",
        "held.example.org",
        "HELD.example.com",
        "a.b.c.d.example.com",
    ] {
        let (status, body) =
            update_domain_name(&emulator, absent, &json!({"routingMode": "API_MAPPING_ONLY"})).await;
        assert_eq!(
            status, 404,
            "case {absent:?}: expected 404 from UpdateDomainName, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "NotFoundException",
            "case {absent:?}: expected NotFoundException from UpdateDomainName, got {body}"
        );

        let (status, body) = get_domain_name(&emulator, absent).await;
        assert_eq!(
            status, 404,
            "case {absent:?}: expected 404 from GetDomainName, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "NotFoundException",
            "case {absent:?}: expected NotFoundException from GetDomainName, got {body}"
        );
    }

    // A refused update persists nothing: the store still holds exactly the one name.
    let (status, held) = get_domain_name(&emulator, "held.example.com").await;
    assert_eq!(
        status, 200,
        "expected the held domain name to survive the refusals, got {status} {held}"
    );
    assert_eq!(
        as_str(&held, "routingMode"),
        DEFAULT_ROUTING_MODE,
        "expected the held domain name to be untouched by the refused updates, got {held}"
    );
}

#[tokio::test]
async fn a_name_the_account_already_holds_is_refused() {
    let emulator = emulator();
    // The rows vary what the second create asks for: the same request, an entry naming
    // nothing itself, a different configuration, and different tags. None of them may
    // replace the stored record.
    let rows = [
        ("the same request", json!({})),
        (
            "an entry naming nothing itself",
            json!({"domainNameConfigurations": [{}]}),
        ),
        (
            "a different configuration",
            json!({"domainNameConfigurations": [{"certificateArn": certificate_arn("other")}]}),
        ),
        ("different tags", json!({"tags": {"owner": "second"}})),
    ];
    let domain = "taken.example.com";
    // The request every row starts from: the name, one endpoint configuration and tags.
    let base = json!({
        "domainName": domain,
        "domainNameConfigurations": [{"certificateArn": certificate_arn("taken")}],
        "tags": {"owner": "first"}
    });
    let first = created(&emulator, "first create", &base).await;

    for (label, extra) in rows {
        let mut request = base.clone();
        for (member, value) in extra.as_object().expect("the extra members") {
            request[member] = value.clone();
        }
        let (status, body) = create_domain_name(&emulator, &request).await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected 400 from a second CreateDomainName, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {label:?}: expected BadRequestException, got {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            DOMAIN_NAME_ALREADY_EXISTS_MESSAGE,
            "case {label:?}: expected the refusal {DOMAIN_NAME_ALREADY_EXISTS_MESSAGE:?}, \
             got {body}"
        );

        let (_, read) = get_domain_name(&emulator, domain).await;
        assert_eq!(
            read, first,
            "case {label:?}: expected the stored record to survive the conflict, got {read}"
        );
    }
}

#[tokio::test]
async fn a_migrating_domain_name_refuses_a_further_update_until_the_migration_is_due() {
    let emulator = emulator();
    let domain = "migrating.example.com";
    created(
        &emulator,
        "seed",
        &json!({
            "domainName": domain,
            "domainNameConfigurations": [{"certificateArn": certificate_arn("before")}]
        }),
    )
    .await;

    let (status, patched) = update_domain_name(
        &emulator,
        domain,
        &json!({"domainNameConfigurations": [{"certificateArn": certificate_arn("after")}]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from the migrating UpdateDomainName, got {status} {patched}"
    );
    assert_eq!(
        configuration_member(&patched, 0, "domainNameStatus"),
        DOMAIN_NAME_STATUS_UPDATING,
        "expected the endpoint change to report {DOMAIN_NAME_STATUS_UPDATING}, got {patched}"
    );

    // "If the status is UPDATING, the domain cannot be modified further until the
    // existing operation is complete" — every further patch is refused, whichever member
    // it names, and the record keeps the configuration the accepted patch left.
    for patch in [
        json!({"routingMode": "API_MAPPING_ONLY"}),
        json!({"mutualTlsAuthentication": {"truststoreUri": "s3://truststores/mid.pem"}}),
        json!({"domainNameConfigurations": [{"certificateArn": certificate_arn("third")}]}),
    ] {
        let (status, body) = update_domain_name(&emulator, domain, &patch).await;
        assert_eq!(
            status, 409,
            "case {patch}: expected 409 while the domain name is \
             {DOMAIN_NAME_STATUS_UPDATING}, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ConflictException",
            "case {patch}: expected ConflictException, got {body}"
        );
    }
    let (_, mid) = get_domain_name(&emulator, domain).await;
    assert_eq!(
        configuration_member(&mid, 0, "certificateArn"),
        certificate_arn("after"),
        "expected the refused patches to leave the migrating configuration alone, got {mid}"
    );

    // The migration is owed by the CLOCK, so once its span has passed the same read
    // reports `AVAILABLE` without anything having been called in between, and the domain
    // name takes updates again. Polled for the CONDITION under a deadline far above the
    // span the service owes it, so nothing here depends on how fast the host runs.
    let deadline = std::time::Instant::now() + MIGRATION_DEADLINE;
    let settled = loop {
        let (status, settled) = get_domain_name(&emulator, domain).await;
        assert_eq!(
            status, 200,
            "expected 200 from GetDomainName while the migration settles, got {status} \
             {settled}"
        );
        if configuration_member(&settled, 0, "domainNameStatus") == DOMAIN_NAME_STATUS_AVAILABLE {
            break settled;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected the migration ({DOMAIN_NAME_MIGRATION:?}) to come due and report \
             {DOMAIN_NAME_STATUS_AVAILABLE} within {MIGRATION_DEADLINE:?}, got {settled}"
        );
        sleep(MIGRATION_POLL_INTERVAL).await;
    };
    assert_eq!(
        configuration_member(&settled, 0, "domainNameStatus"),
        DOMAIN_NAME_STATUS_AVAILABLE,
        "expected the migration to come due and report \
         {DOMAIN_NAME_STATUS_AVAILABLE}, got {settled}"
    );
    let (status, accepted) =
        update_domain_name(&emulator, domain, &json!({"routingMode": "API_MAPPING_ONLY"})).await;
    assert_eq!(
        status, 200,
        "expected a settled domain name to take an update, got {status} {accepted}"
    );
    assert_eq!(
        as_str(&accepted, "routingMode"),
        "API_MAPPING_ONLY",
        "expected the update accepted after the migration to apply, got {accepted}"
    );
}

#[tokio::test]
async fn the_reported_statuses_are_the_modelled_catalog() {
    // Both statuses this service reports are values of the modelled `DomainNameStatus`
    // enum, so neither can drift into a spelling no client would accept.
    for status in [DOMAIN_NAME_STATUS_AVAILABLE, DOMAIN_NAME_STATUS_UPDATING] {
        assert!(
            DOMAIN_NAME_STATUS_VALUES.contains(&status),
            "case {status:?}: expected a value of DOMAIN_NAME_STATUS_VALUES \
             ({DOMAIN_NAME_STATUS_VALUES:?})"
        );
    }
    assert!(
        ROUTING_MODE_VALUES.contains(&DEFAULT_ROUTING_MODE),
        "expected the default routing mode {DEFAULT_ROUTING_MODE:?} to be a value of \
         ROUTING_MODE_VALUES ({ROUTING_MODE_VALUES:?})"
    );
}

#[tokio::test]
async fn a_domain_name_outside_the_documented_length_is_refused() {
    let emulator = emulator();
    // `StringWithLengthBetween1And512` is "A string with a length between [1-512]", so
    // each published bound is exercised and so is one character past each of them.
    let at_bound = "x".repeat(DOMAIN_NAME_MAX_LENGTH);
    let past_bound = "x".repeat(DOMAIN_NAME_MAX_LENGTH + 1);
    let accepted = [("1 character", "y"), ("512 characters", at_bound.as_str())];
    let refused = [
        ("0 characters", ""),
        ("513 characters", past_bound.as_str()),
    ];

    // Every row carries one endpoint configuration, so the name's length is the only
    // thing that can decide the answer.
    let configurations = json!([{"certificateArn": certificate_arn("length")}]);
    for (label, domain) in accepted {
        let (status, body) = create_domain_name(
            &emulator,
            &json!({"domainName": domain, "domainNameConfigurations": configurations.clone()}),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {label:?}: expected 201 from CreateDomainName, got {status} {body}"
        );
    }
    for (label, domain) in refused {
        let (status, body) = create_domain_name(
            &emulator,
            &json!({"domainName": domain, "domainNameConfigurations": configurations.clone()}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected 400 from CreateDomainName, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {label:?}: expected BadRequestException, got {body}"
        );
    }
}

#[tokio::test]
async fn a_request_cannot_set_the_service_computed_domain_name_status() {
    let emulator = emulator();
    created(
        &emulator,
        "seed",
        &json!({
            "domainName": "computed.example.com",
            "domainNameConfigurations": [{"certificateArn": certificate_arn("computed")}]
        }),
    )
    .await;

    // `DomainNameStatus` reports the migration the SERVICE owes, so a request naming it
    // is naming a value it does not own — on the create and on the patch alike, and at
    // whatever position in the list it appears.
    let rows = [
        (
            "on a create",
            "POST",
            "/v2/domainnames".to_string(),
            json!({
                "domainName": "forced.example.com",
                "domainNameConfigurations": [{"domainNameStatus": DOMAIN_NAME_STATUS_AVAILABLE}]
            }),
        ),
        (
            "on a patch, second entry",
            "PATCH",
            "/v2/domainnames/computed.example.com".to_string(),
            json!({
                "domainNameConfigurations": [
                    {"certificateArn": certificate_arn("first")},
                    {"domainNameStatus": DOMAIN_NAME_STATUS_UPDATING}
                ]
            }),
        ),
    ];
    for (label, method, uri, body) in rows {
        let (status, answered) = call(&emulator, method, &uri, Some(body)).await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected 400 for a request setting domainNameStatus, got \
             {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "BadRequestException",
            "case {label:?}: expected BadRequestException, got {answered}"
        );
    }

    // Neither refusal wrote anything: the seeded record is untouched and the create's
    // name was never taken.
    let (status, read) = get_domain_name(&emulator, "computed.example.com").await;
    assert_eq!(
        status, 200,
        "expected the seeded record to survive, got {status} {read}"
    );
    assert_eq!(
        configuration_member(&read, 0, "certificateArn"),
        certificate_arn("computed"),
        "expected the seeded configuration to be untouched, got {read}"
    );
    let (status, _) = get_domain_name(&emulator, "forced.example.com").await;
    assert_eq!(
        status, 404,
        "expected the refused create to have persisted nothing, got {status}"
    );
}

/// One row of the representation table: the Region the request is signed for, the name, the
/// configurations and the tags the create attaches.
struct RepresentationCase {
    case: &'static str,
    region: &'static str,
    domain: &'static str,
    configurations: Value,
    /// The `tags` member of the request, or `None` for a request that attaches none.
    tags: Option<Value>,
}

/// The rows the ARN rule and the tags rule are asserted over: they differ along the Region
/// the request is signed for, the form of the domain name, how many endpoint configurations
/// the resource carries, and whether the create attached tags.
fn representation_cases() -> Vec<RepresentationCase> {
    vec![
        RepresentationCase {
            case: "a dotted name, one configuration, no tags",
            region: "us-west-2",
            domain: "representation.example.com",
            configurations: json!([{}]),
            tags: None,
        },
        RepresentationCase {
            case: "a hyphenated name in another Region, three configurations, no tags",
            region: "ap-southeast-2",
            domain: "edge-api-two.example.com",
            configurations: json!([
                { "endpointType": "REGIONAL" },
                { "endpointType": "EDGE" },
                { "endpointType": "REGIONAL", "securityPolicy": "TLS_1_2" }
            ]),
            tags: None,
        },
        RepresentationCase {
            case: "the shortest name the target shape allows, with one tag",
            region: "eu-west-3",
            domain: "a",
            configurations: json!([{}]),
            tags: Some(json!({ "team": "edge" })),
        },
        RepresentationCase {
            case: "a deep name with several tags",
            region: "sa-east-1",
            domain: "gateway.internal.service.example.com",
            configurations: json!([{ "endpointType": "EDGE" }]),
            tags: Some(json!({ "team": "edge", "env": "test" })),
        },
    ]
}

/// Create the domain name one representation row describes and answer the created document.
async fn create_for_representation(
    emulator: &MultiServiceRouter,
    row: &RepresentationCase,
) -> Value {
    let mut request = json!({
        "domainName": row.domain,
        "domainNameConfigurations": row.configurations.clone()
    });
    if let Some(tags) = &row.tags {
        request["tags"] = tags.clone();
    }
    let (status, body) = call_in_region(
        emulator,
        row.region,
        "POST",
        "/v2/domainnames",
        Some(request),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {:?}: expected 201 from CreateDomainName, got {status} {body}",
        row.case
    );
    body
}

/// The one item of `GetDomainNames` that names `domain`.
fn listed_domain_name(case: &str, page: &Value, domain: &str) -> Value {
    page.get("items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| as_str(item, "domainName") == domain)
                .cloned()
        })
        .unwrap_or_else(|| panic!("case {case:?}: GetDomainNames must list {domain}, got {page}"))
}

/// Every writer and every reader of a custom domain name reports the account-less ARN of
/// the resource, and the ARN does not move with the Region a later reader signs for.
#[tokio::test]
async fn a_custom_domain_name_is_named_by_an_account_less_api_gateway_arn() {
    let emulator = emulator();
    // A Region no row is created in, so a read signed for it cannot be the Region the ARN
    // reports by accident.
    let elsewhere = "us-east-2";

    for row in representation_cases() {
        let case = row.case;
        let created = create_for_representation(&emulator, &row).await;
        let expected = expected_domain_name_arn(row.region, row.domain);
        assert_eq!(
            as_str(&created, "domainNameArn"),
            expected,
            "case {case:?}: expected CreateDomainName to report {expected:?}, got {created}"
        );

        // The single reader, signed for the creating Region and for another one: an ARN
        // names the Region its resource was created in.
        for reader_region in [row.region, elsewhere] {
            let (status, read) = call_in_region(
                &emulator,
                reader_region,
                "GET",
                &format!("/v2/domainnames/{}", row.domain),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} (read in {reader_region:?}): expected 200 from \
                 GetDomainName, got {status} {read}"
            );
            assert_eq!(
                as_str(&read, "domainNameArn"),
                expected,
                "case {case:?} (read in {reader_region:?}): expected the ARN the create \
                 answered, got {read}"
            );
        }

        // The collection reader, which projects the same records.
        let (status, listed) =
            call_in_region(&emulator, row.region, "GET", "/v2/domainnames", None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetDomainNames, got {status} {listed}"
        );
        assert_eq!(
            as_str(&listed_domain_name(case, &listed, row.domain), "domainNameArn"),
            expected,
            "case {case:?}: expected GetDomainNames to report the same ARN, got {listed}"
        );

        // And a patch, signed for yet another Region, leaves the ARN where it is.
        let (status, patched) = call_in_region(
            &emulator,
            elsewhere,
            "PATCH",
            &format!("/v2/domainnames/{}", row.domain),
            Some(json!({ "routingMode": "API_MAPPING_ONLY" })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from UpdateDomainName, got {status} {patched}"
        );
        assert_eq!(
            as_str(&patched, "domainNameArn"),
            expected,
            "case {case:?}: expected the patch to leave the ARN alone, got {patched}"
        );
    }
}

/// The create answers the tags it was asked to attach — no member at all for a request that
/// attached none — while every reader of the resource reports its collection.
#[tokio::test]
async fn the_create_answers_the_tags_it_attached_and_every_reader_the_collection() {
    let emulator = emulator();

    for row in representation_cases() {
        let case = row.case;
        let created = create_for_representation(&emulator, &row).await;
        assert_eq!(
            created.get("tags"),
            row.tags.as_ref(),
            "case {case:?}: expected CreateDomainName to answer the tags it attached \
             ({:?}), got {created}",
            row.tags
        );

        // The collection the resource carries, which is the empty map for a resource
        // nothing tagged.
        let collection = row.tags.clone().unwrap_or_else(|| json!({}));
        let (status, read) = call_in_region(
            &emulator,
            row.region,
            "GET",
            &format!("/v2/domainnames/{}", row.domain),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetDomainName, got {status} {read}"
        );
        assert_eq!(
            read.get("tags"),
            Some(&collection),
            "case {case:?}: expected GetDomainName to report the resource's collection \
             ({collection}), got {read}"
        );

        let (status, listed) =
            call_in_region(&emulator, row.region, "GET", "/v2/domainnames", None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetDomainNames, got {status} {listed}"
        );
        assert_eq!(
            listed_domain_name(case, &listed, row.domain).get("tags"),
            Some(&collection),
            "case {case:?}: expected GetDomainNames to report the same collection, got \
             {listed}"
        );

        let (status, patched) = call_in_region(
            &emulator,
            row.region,
            "PATCH",
            &format!("/v2/domainnames/{}", row.domain),
            Some(json!({ "routingMode": "API_MAPPING_ONLY" })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from UpdateDomainName, got {status} {patched}"
        );
        assert_eq!(
            patched.get("tags"),
            Some(&collection),
            "case {case:?}: expected the patch to report the same collection, got {patched}"
        );
    }
}
