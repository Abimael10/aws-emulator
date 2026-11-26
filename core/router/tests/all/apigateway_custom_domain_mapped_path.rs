// SPDX-License-Identifier: CC0-1.0

//! What a request addressed to a REST api's CUSTOM DOMAIN NAME is served by — at every
//! address that domain name is reached at, and however the client stated the host.
//!
//! The rule under test, one level above the recorded case (one regional domain name, one base
//! path mapping, one signed `GET` of a mapped path):
//!
//! > A `BasePathMapping` "relates a path to an API", so a request that arrived on a custom
//! > domain name of the account is served by the `{restApiId, stage}` of the mapping that
//! > answers its path — and WHICH api that is comes out of the account's own records, since
//! > the request names neither. A custom domain name is reached at the name itself, which a
//! > caller points a DNS record at, and at the address API Gateway published for that record
//! > to target: `regionalDomainName` for a regional endpoint ("the domain name associated
//! > with the regional endpoint … you set up this association by adding a DNS record that
//! > points the custom domain name to this regional domain name") and
//! > `distributionDomainName` for an edge-optimized one ("the domain name of the Amazon
//! > CloudFront distribution associated with this custom domain name"). A client on the
//! > machine this emulator runs on resolves none of those hosts, so it reaches the emulator's
//! > own endpoint and carries in the PATH what it cannot carry in the host — the reserved
//! > namespace every data plane of this emulator is addressed through. One request therefore
//! > reaches ONE api and one stage whichever endpoint type the record carries and whichever
//! > of those spellings the client used, and the stage's own `authorizationType` then decides
//! > it: an `AWS_IAM` method serves a request carrying a SigV4 credential and refuses one
//! > that carries none.
//!
//! Three crates have to agree for that to hold — Route 53 publishes the challenge that proves
//! the name, Certificate Manager issues the certificate the domain name is created with, and
//! API Gateway publishes the address and serves the mapped stage — so this suite drives the
//! WHOLE assembled router over one engine and creates every resource through its own API.
//!
//! The rows span what the rule generalises over: the endpoint type of the record (regional
//! and edge-optimized, which is the type a create that names none carries), the spelling of
//! the host (the domain name, the address the record published, and each of those carried in
//! the reserved path namespace), the base path the mapping answers under (a named one and
//! `(none)`), and the credential the request carries (one the account published, and none at
//! all).

use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::apigateway_iam_authorized_invoke::{
    as_str, control_plane, credential_scope, dispatch, Answer, BOOT_STACK_SIZE, REGION,
};
use super::route53_zone_discovery::{create_zone, publish_record};

/// The public hosted zone the account holds, and so the zone every certificate of this suite
/// is validated in and every custom domain name of it lives under.
const ZONE: &str = "mapped-front-door.example.com";

/// The two custom domain names: one per endpoint type a public alias is published for.
const REGIONAL_DOMAIN: &str = "regional.mapped-front-door.example.com";
const EDGE_DOMAIN: &str = "edge.mapped-front-door.example.com";

/// The stage the api is deployed to, the resource its one method answers under, and the base
/// path the keyed mapping answers.
const STAGE: &str = "prod";
const RESOURCE: &str = "items";
const BASE_PATH: &str = "v1";

/// The body the deployed `MOCK` method answers with, so a `200` is the STAGE's own answer
/// rather than anything this suite sent.
const SERVED_BODY: &str = r#"{"served":"items"}"#;

/// The access key id this emulator publishes, which speaks for the account's own identity:
/// what a client of the account signs an invoke with.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The signing name an invoke of a deployed stage is signed under — the service AWS publishes
/// the invoke endpoint and the `execute-api:Invoke` action under, not this service's
/// control-plane name.
const INVOKE_SIGNING_NAME: &str = "execute-api";

/// The reserved path namespace a client carries a host it resolves nowhere in.
const NAMESPACE: &str = "_aws";

/// The emulator's own endpoint, which such a client addresses instead.
const EMULATOR_ENDPOINT: &str = "127.0.0.1:9297";

/// The status and sentence an `AWS_IAM` method reached with no credential at all is answered
/// with.
const MISSING_TOKEN: (u16, &str) = (403, "Missing Authentication Token");

/// The `domainNameStatus` a custom domain name is usable in, and the certificate status a
/// domain name may be created with.
const AVAILABLE: &str = "AVAILABLE";
const ISSUED: &str = "ISSUED";

/// How often, and for how long, the suite asks whether a transition the clock owes has come
/// due (the issuance a published challenge owes, the endpoint an edge-optimized record waits
/// for). A generous budget: a loaded machine is slower than the spans these services choose
/// without the rule being any different.
const POLL: Duration = Duration::from_millis(25);
const POLLS: usize = 2_000;

/// Call one Certificate Manager operation over its awsJson1_1 wire, requiring the 200 every
/// operation here must answer.
async fn acm(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let target = format!("CertificateManager.{operation}");
    let authorization = credential_scope(ACCOUNT_KEY_ID, "acm");
    let answer = dispatch(
        router,
        &format!("acm.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &target),
            ("authorization", &authorization),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup: acm {operation} must answer 200, got {}",
        answer.summary()
    );
    answer.json()
}

/// Request a DNS-validated public certificate for `name`, publish every challenge it hands
/// out in `zone`, and answer the ARN once Certificate Manager reports it `ISSUED`.
///
/// The challenge records are READ from the certificate rather than composed here, and the
/// issuance is reached by polling: the transition is one the clock owes once the proof is
/// published, exactly as a client's own waiter sees it.
async fn issued_certificate(router: &MultiServiceRouter, zone: &str, name: &str) -> String {
    let requested = acm(
        router,
        "RequestCertificate",
        &json!({ "DomainName": name, "ValidationMethod": "DNS" }),
    )
    .await;
    let arn = as_str("RequestCertificate", &requested, "CertificateArn");
    let pending = acm(
        router,
        "DescribeCertificate",
        &json!({ "CertificateArn": arn }),
    )
    .await;
    let validations = pending["Certificate"]["DomainValidationOptions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !validations.is_empty(),
        "setup: a DNS-validated request carries a challenge per covered name, got {pending}"
    );
    for validation in &validations {
        let record = &validation["ResourceRecord"];
        let (Some(record_name), Some(kind), Some(value)) = (
            record["Name"].as_str(),
            record["Type"].as_str(),
            record["Value"].as_str(),
        ) else {
            panic!("setup: every pending validation carries its record: {pending}");
        };
        publish_record(router, name, zone, record_name, kind, "60", value).await;
    }
    let mut status = String::new();
    for _ in 0..POLLS {
        let read = acm(
            router,
            "DescribeCertificate",
            &json!({ "CertificateArn": arn }),
        )
        .await;
        status = read["Certificate"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == ISSUED {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        status, ISSUED,
        "setup: the certificate for {name} must reach {ISSUED}, got {status:?}"
    );
    arn
}

/// Create one custom domain name, poll it to [`AVAILABLE`], and answer the record it reports
/// — whose published address is what a caller points its own DNS record at.
async fn available_domain(
    router: &MultiServiceRouter,
    domain_name: &str,
    request: Value,
) -> Value {
    let created = control_plane(
        router,
        &format!("CreateDomainName({domain_name})"),
        "POST",
        "/domainnames",
        Some(request),
        201,
    )
    .await;
    let mut record = created;
    for _ in 0..POLLS {
        if record.get("domainNameStatus").and_then(Value::as_str) == Some(AVAILABLE) {
            break;
        }
        tokio::time::sleep(POLL).await;
        record = control_plane(
            router,
            &format!("GetDomainName({domain_name})"),
            "GET",
            &format!("/domainnames/{domain_name}"),
            None,
            200,
        )
        .await;
    }
    assert_eq!(
        record.get("domainNameStatus").and_then(Value::as_str),
        Some(AVAILABLE),
        "setup: {domain_name} must reach {AVAILABLE}, got {record}"
    );
    record
}

/// One base path mapping of `domain_name` onto `api`'s `STAGE`.
async fn given_mapping(
    router: &MultiServiceRouter,
    domain_name: &str,
    api: &str,
    base_path: &str,
) {
    control_plane(
        router,
        &format!("CreateBasePathMapping({domain_name}, {base_path})"),
        "POST",
        &format!("/domainnames/{domain_name}/basepathmappings"),
        Some(json!({ "basePath": base_path, "restApiId": api, "stage": STAGE })),
        201,
    )
    .await;
}

/// How a client states the host of the custom domain name it is addressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stated {
    /// As the `Host` of a request made at that host, which is what a client that resolved the
    /// name sends.
    AsHost,
    /// Carried in the reserved path namespace of a request made at this emulator's own
    /// endpoint, which is what a client that resolves the host nowhere sends.
    CarriedInThePath,
}

/// Which host of the record a row states: the domain NAME, or the address the record PUBLISHED
/// for a DNS record to target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    TheDomainName,
    ThePublishedAddress,
}

/// One row: the record it addresses, which of that record's hosts it states, how it states it,
/// and the path it asks for under the mapping that answers it.
struct Row {
    case: &'static str,
    /// The custom domain name the row addresses.
    domain_name: &'static str,
    host: Host,
    stated: Stated,
    /// The path a client of the custom domain name asks for.
    path: &'static str,
}

/// The rows: every spelling of every host of both endpoint types, under both base paths.
const ROWS: &[Row] = &[
    Row {
        case: "a regional record addressed by its own name, under a named base path",
        domain_name: REGIONAL_DOMAIN,
        host: Host::TheDomainName,
        stated: Stated::AsHost,
        path: "/v1/items",
    },
    Row {
        case: "a regional record addressed by the address it published",
        domain_name: REGIONAL_DOMAIN,
        host: Host::ThePublishedAddress,
        stated: Stated::AsHost,
        path: "/v1/items",
    },
    Row {
        case: "a regional record whose name a client carried in the path",
        domain_name: REGIONAL_DOMAIN,
        host: Host::TheDomainName,
        stated: Stated::CarriedInThePath,
        path: "/v1/items",
    },
    Row {
        case: "a regional record whose published address a client carried in the path",
        domain_name: REGIONAL_DOMAIN,
        host: Host::ThePublishedAddress,
        stated: Stated::CarriedInThePath,
        path: "/v1/items",
    },
    Row {
        case: "a regional record under the mapping that answers every unclaimed path",
        domain_name: REGIONAL_DOMAIN,
        host: Host::TheDomainName,
        stated: Stated::AsHost,
        path: "/items",
    },
    Row {
        case: "an edge-optimized record addressed by its own name",
        domain_name: EDGE_DOMAIN,
        host: Host::TheDomainName,
        stated: Stated::AsHost,
        path: "/v1/items",
    },
    Row {
        case: "an edge-optimized record addressed by the distribution name it published",
        domain_name: EDGE_DOMAIN,
        host: Host::ThePublishedAddress,
        stated: Stated::AsHost,
        path: "/v1/items",
    },
    Row {
        case: "an edge-optimized record whose name a client carried in the path",
        domain_name: EDGE_DOMAIN,
        host: Host::TheDomainName,
        stated: Stated::CarriedInThePath,
        path: "/v1/items",
    },
    Row {
        case: "an edge-optimized record whose published name a client carried in the path",
        domain_name: EDGE_DOMAIN,
        host: Host::ThePublishedAddress,
        stated: Stated::CarriedInThePath,
        path: "/v1/items",
    },
    Row {
        case: "an edge-optimized record under the mapping that answers every unclaimed path",
        domain_name: EDGE_DOMAIN,
        host: Host::TheDomainName,
        stated: Stated::CarriedInThePath,
        path: "/items",
    },
];

/// Send `row`'s request, carrying `authorization` when the row's credential is one.
async fn ask(
    router: &MultiServiceRouter,
    row: &Row,
    published: &str,
    authorization: Option<&str>,
) -> Answer {
    let addressed = match row.host {
        Host::TheDomainName => row.domain_name,
        Host::ThePublishedAddress => published,
    };
    let (host, path) = match row.stated {
        Stated::AsHost => (addressed.to_string(), row.path.to_string()),
        Stated::CarriedInThePath => (
            EMULATOR_ENDPOINT.to_string(),
            format!("/{NAMESPACE}/{addressed}{}", row.path),
        ),
    };
    let headers: Vec<(&str, &str)> = authorization
        .map(|value| vec![("authorization", value)])
        .unwrap_or_default();
    dispatch(router, &host, "GET", &path, &headers, Vec::new()).await
}

#[test]
fn a_mapped_path_of_a_custom_domain_name_is_served_at_every_address_it_is_reached_at() {
    std::thread::Builder::new()
        .name("apigateway-custom-domain-mapped-path".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                // Signature verification belongs to the emulator, not to this check: the rows
                // are about WHICH stage answers a credential, not about SigV4 itself.
                router.set_request_authentication(false);

                // 1. The zone the account holds, and one issued certificate per domain name.
                let zone = create_zone(&router, "the account's own zone", ZONE, false, ZONE).await;
                let regional_certificate =
                    issued_certificate(&router, &zone, REGIONAL_DOMAIN).await;
                let edge_certificate = issued_certificate(&router, &zone, EDGE_DOMAIN).await;

                // 2. The api behind both domain names: one `AWS_IAM` method over a `MOCK`
                //    integration, deployed to one stage.
                let api = control_plane(
                    &router,
                    "CreateRestApi",
                    "POST",
                    "/restapis",
                    Some(json!({ "name": "mapped-front-door" })),
                    201,
                )
                .await;
                let api_id = as_str("CreateRestApi", &api, "id");
                let root = as_str("CreateRestApi", &api, "rootResourceId");
                let resource = control_plane(
                    &router,
                    "CreateResource",
                    "POST",
                    &format!("/restapis/{api_id}/resources/{root}"),
                    Some(json!({ "pathPart": RESOURCE })),
                    201,
                )
                .await;
                let resource_id = as_str("CreateResource", &resource, "id");
                let method = format!("/restapis/{api_id}/resources/{resource_id}/methods/GET");
                control_plane(
                    &router,
                    "PutMethod",
                    "PUT",
                    &method,
                    Some(json!({ "authorizationType": "AWS_IAM" })),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "PutIntegration",
                    "PUT",
                    &format!("{method}/integration"),
                    Some(json!({ "type": "MOCK" })),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "PutMethodResponse",
                    "PUT",
                    &format!("{method}/responses/200"),
                    Some(json!({})),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "PutIntegrationResponse",
                    "PUT",
                    &format!("{method}/integration/responses/200"),
                    Some(json!({
                        "responseTemplates": { "application/json": SERVED_BODY }
                    })),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "CreateDeployment",
                    "POST",
                    &format!("/restapis/{api_id}/deployments"),
                    Some(json!({ "stageName": STAGE })),
                    201,
                )
                .await;

                // 3. The two custom domain names, each with the two mappings the rows ask
                //    under: a named base path and the one that answers every path no base
                //    path claims.
                let regional = available_domain(
                    &router,
                    REGIONAL_DOMAIN,
                    json!({
                        "domainName": REGIONAL_DOMAIN,
                        "regionalCertificateArn": regional_certificate,
                        "endpointConfiguration": { "types": ["REGIONAL"] },
                        "securityPolicy": "TLS_1_2",
                    }),
                )
                .await;
                // The address a caller points its own DNS record at — the member a regional
                // record publishes.
                let regional_published = as_str("GetDomainName", &regional, "regionalDomainName");
                // An edge-optimized record is what a create naming NO endpoint type carries,
                // which is API Gateway's default: `certificateArn` is the member that
                // endpoint's certificate travels in.
                let edge = available_domain(
                    &router,
                    EDGE_DOMAIN,
                    json!({
                        "domainName": EDGE_DOMAIN,
                        "certificateArn": edge_certificate,
                    }),
                )
                .await;
                let edge_published = as_str("GetDomainName", &edge, "distributionDomainName");
                for domain_name in [REGIONAL_DOMAIN, EDGE_DOMAIN] {
                    given_mapping(&router, domain_name, &api_id, BASE_PATH).await;
                    given_mapping(&router, domain_name, &api_id, "(none)").await;
                }

                // 4. Every row, with the credential the account published and with none.
                let authorization = credential_scope(ACCOUNT_KEY_ID, INVOKE_SIGNING_NAME);
                let expected: Value =
                    serde_json::from_str(SERVED_BODY).expect("the served body is JSON");
                for row in ROWS {
                    let published = match row.domain_name {
                        EDGE_DOMAIN => edge_published.as_str(),
                        _ => regional_published.as_str(),
                    };
                    let served = ask(&router, row, published, Some(&authorization)).await;
                    assert_eq!(
                        served.status,
                        200,
                        "case {:?}: expected the mapped stage's 200, got {}",
                        row.case,
                        served.summary()
                    );
                    assert_eq!(
                        served.json(),
                        expected,
                        "case {:?}: expected the integration's body {expected}, got {}",
                        row.case,
                        served.summary()
                    );

                    // The same request carrying no credential at all: the method is
                    // `AWS_IAM`, so what the front door reached is the stage's own refusal
                    // and not an answer about the addressing.
                    let refused = ask(&router, row, published, None).await;
                    assert_eq!(
                        (refused.status, refused.message().as_str()),
                        MISSING_TOKEN,
                        "case {:?}: expected {MISSING_TOKEN:?} for a request carrying no \
                         credential, got {}",
                        row.case,
                        refused.summary()
                    );
                }

                // 5. And the published addresses really are the record's own: the member each
                //    endpoint type publishes is the one the rows above addressed, and a
                //    regional record publishes an address a client can DIAL.
                assert!(
                    regional_published
                        .parse::<std::net::SocketAddr>()
                        .is_ok(),
                    "expected regionalDomainName to be an address a client can dial, got \
                     {regional_published:?}"
                );
                assert!(
                    edge_published.ends_with(".cloudfront.net"),
                    "expected distributionDomainName to name the distribution API Gateway \
                     put in front of the record, got {edge_published:?}"
                );
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
