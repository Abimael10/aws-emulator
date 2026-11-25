// SPDX-License-Identifier: CC0-1.0

//! Finding one of the account's own DNS zones by tag, proving a name of it to Certificate
//! Manager, and serving an AppSync custom domain on that name.
//!
//! The rule under test: **a hosted zone the account created is enumerated by
//! `ListHostedZones` — across every `Marker`/`NextMarker` page, whatever page size the
//! caller asks for — reporting the `Config/PrivateZone` flag it was created with, and
//! `ListTagsForResource('hostedzone', <its id>)` reports exactly the tags written on it;
//! so a client that pages the zones, keeps the public ones and reads each one's tags
//! resolves the zone carrying a selector tag, publishes in it the DNS challenge
//! Certificate Manager handed out for a name of that zone, and AppSync then serves a
//! custom domain on that name through its modelled lifecycle: `CreateDomainName` and
//! `GetDomainName` report the configuration AppSync stored, `GetApiAssociation` answers
//! `NotFoundException` (404) while the domain carries no API, `AssociateApi` maps one API
//! onto it and the read reaches `SUCCESS` naming that API, `DisassociateApi` leaves the
//! domain carrying none again, and `DeleteDomainName` leaves the name a 404.**
//!
//! Three crates have to agree for the rule to hold (Route 53 enumerates, tags and
//! publishes, Certificate Manager hands out the challenge and issues on the published
//! proof, AppSync resolves that certificate and serves the domain), so the suite lives
//! here: a crate-local test only ever sees one side of it.
//!
//! The rows differ along what the rule generalises over: how many labels the zone's apex
//! has and how many the name the domain is created under adds to it, how many tags the
//! zone carries and the form of their keys and values, and whether the certificate is
//! issued for that name itself or for a wildcard covering it. The enumeration is paged at
//! every page size from one zone per page up to one page for the whole account. Beside
//! the rows the account holds two zones no tag-driven discovery may select — a PRIVATE
//! zone carrying the selector tag, and a public zone carrying other tags only.
//!
//! The zone id, the apex and the selector tag are the ACCOUNT's values, never part of the
//! API: WHICH tag a client selects its zones by is its own choice, and every value the
//! certificate and the domain are judged by — the ARN, the challenge record, the endpoint
//! AppSync mints — is read back from the service that minted it rather than written here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The XML element readers this crate writes ONCE (`tests/all/query_wire.rs`) and the
// Route 53 tag-discovery walk this crate writes ONCE
// (`tests/all/route53_zone_discovery.rs`): "Nothing new should write another one — call
// this module."
use super::route53_zone_discovery::{
    self, create_zone, publish_record, tag_selected_public_zones, tag_zone, zone_tags, ListedZone,
};

/// The Region the AppSync calls are signed for, and the one a custom domain's ARN is
/// minted in.
const APPSYNC_REGION: &str = "us-west-2";

/// The Region a custom domain's distribution reads its certificate from, which is where
/// the certificate is requested (`state::CERTIFICATE_REGION` is AppSync's own copy).
const CERTIFICATE_REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The record type Certificate Manager's DNS challenge is published as ("the CNAME record
/// that you add to your DNS database for domain validation",
/// `DomainValidation$ResourceRecord`).
const CNAME: &str = "CNAME";

/// The `TTL`, in seconds, this suite publishes the challenge record with. The TTL is the
/// publisher's choice; nothing of the rule turns on it.
const CHALLENGE_TTL: &str = "300";

/// The `CertificateStatus` values the walk passes through
/// (`smithy_query(query="shape", shape="com.amazonaws.acm#CertificateStatus")`).
const PENDING_VALIDATION: &str = "PENDING_VALIDATION";
const ISSUED: &str = "ISSUED";

/// The `AssociationStatus` a mapping carries while AppSync is deploying it, and the one it
/// reaches once the deployment is due
/// (`smithy_query(query="shape", shape="com.amazonaws.appsync#AssociationStatus")`).
const PROCESSING: &str = "PROCESSING";
const SUCCESS: &str = "SUCCESS";

/// The wire error code every AppSync refusal here carries, and AppSync's fixed sentences
/// for the three conditions this walk meets: a domain that carries no API association, a
/// domain name this account does not hold on a READ, and a certificate that cannot serve
/// the requested name.
const NOT_FOUND: &str = "NotFoundException";
const BAD_REQUEST: &str = "BadRequestException";
const ASSOCIATION_NOT_FOUND: &str = "Domain name is not associated.";
const DOMAIN_NOT_FOUND_ON_READ: &str = "Domain name not found";
const CERTIFICATE_INVALID: &str = "Certificate is invalid.";

/// The suffix of every `appsyncDomainName`: a custom domain is served through a content
/// delivery distribution's own domain.
const APPSYNC_DOMAIN_SUFFIX: &str = ".cloudfront.net";

/// The Route 53 hosted zone id every content delivery distribution — and therefore every
/// AppSync custom domain — resolves through. It is a single global value, published by AWS
/// in the Route 53 developer guide's "Values that you specify when you create or update
/// Amazon Route 53 records" (the alias `HostedZoneId` to give for a CloudFront
/// distribution), and the AppSync crate carries its own copy of it
/// (`state::CLOUDFRONT_HOSTED_ZONE_ID`) — so it is a documented constant rather than a
/// value taken from one recording.
const CLOUDFRONT_HOSTED_ZONE_ID: &str = "Z2FDTNDATAQYW2";

/// The two refusals the ROUTER itself answers, which no call here may provoke.
const ROUTER_REFUSALS: &[&str] = &["AmbiguousService", "UnknownService"];

/// How long a poll goes on waiting for an outcome the clock already owes, and how often it
/// looks again.
///
/// Every outcome waited for here is owed on a span the services themselves fix (the
/// issuance Certificate Manager owes once it has observed the proof, the deployment
/// AppSync owes a fresh mapping), so the deadline is generous: it is only here so that a
/// failure is a failure rather than a hang, and a loaded machine takes more polls rather
/// than a different answer.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The tag a discovery selects a zone by. Both halves are the ACCOUNT's: a selector tag is
/// a value a client writes, so no part of it comes from the API.
const SELECTOR: (&str, &str) = ("dns-baseline", "true");

/// Which name the certificate of a row is issued for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CertifiedName {
    /// The certificate names the custom domain's own name.
    TheNameItself,
    /// The certificate names a wildcard one label above it, which covers that name.
    AWildcardCoveringIt,
}

/// One zone of the account whose apex a custom domain is served under.
struct DomainRow {
    case: &'static str,
    /// The apex the PUBLIC hosted zone is created for. A reserved top-level label (RFC
    /// 2606) the public DNS delegates to nobody, so the account's own public zone is the
    /// authority for every name under it — which is what lets the challenge published in
    /// it prove the name.
    apex: &'static str,
    /// The labels the custom domain's name adds below the apex.
    label: &'static str,
    /// The tags written on the zone beside [`SELECTOR`].
    extra_tags: &'static [(&'static str, &'static str)],
    certified: CertifiedName,
}

/// The zones a discovery must select, spanning the forms the rule generalises over.
const DOMAIN_ROWS: &[DomainRow] = &[
    DomainRow {
        case: "a two-label apex, one label under it, certified by name",
        apex: "appsync-apex.example",
        label: "api",
        extra_tags: &[],
        certified: CertifiedName::TheNameItself,
    },
    DomainRow {
        case: "a three-label apex, two labels under it, certified by name",
        apex: "edge.appsync-second.example",
        label: "shop.eu",
        extra_tags: &[("Owner", "dns-team"), ("cost:centre", "4815")],
        certified: CertifiedName::TheNameItself,
    },
    DomainRow {
        case: "a two-label apex, one label under it, certified by a wildcard covering it",
        apex: "appsync-wildcard.example",
        label: "store",
        extra_tags: &[("Env", "prod")],
        certified: CertifiedName::AWildcardCoveringIt,
    },
];

/// One zone the account holds that no tag-driven discovery of PUBLIC zones may select:
/// what it is, its apex, whether it is private, and the tags it carries.
struct DecoyRow {
    case: &'static str,
    apex: &'static str,
    private: bool,
    tags: &'static [(&'static str, &'static str)],
}

const DECOY_ROWS: &[DecoyRow] = &[
    DecoyRow {
        case: "a private zone carrying the selector tag",
        apex: "internal.appsync-private.example",
        private: true,
        tags: &[SELECTOR, ("Env", "dev")],
    },
    DecoyRow {
        case: "a public zone carrying other tags only",
        apex: "appsync-unselected.example",
        private: false,
        tags: &[("Owner", "dns-team")],
    },
    DecoyRow {
        case: "a public zone carrying no tag at all",
        apex: "appsync-untagged.example",
        private: false,
        tags: &[],
    },
];

/// The page sizes the enumeration is walked at: one zone per page, two, one short of the
/// account's zone count, exactly it, one more, and no `maxitems` at all (the service's own
/// page size).
fn page_sizes() -> Vec<Option<i32>> {
    let held = (DOMAIN_ROWS.len() + DECOY_ROWS.len()) as i32;
    vec![
        Some(1),
        Some(2),
        // `maxitems` below one asks for no page at all, which Route 53 refuses, so a
        // table trimmed to a single zone still pages at one.
        Some((held - 1).max(1)),
        Some(held),
        Some(held + 1),
        None,
    ]
}

/// The custom domain's name: the row's labels below its apex.
fn domain_name(row: &DomainRow) -> String {
    format!("{}.{}", row.label, row.apex)
}

/// The name the row's certificate is requested for.
fn certificate_name(row: &DomainRow) -> String {
    match row.certified {
        CertifiedName::TheNameItself => domain_name(row),
        // One label above the domain's own name, which is the wildcard a TLS name match
        // covers that name with.
        CertifiedName::AWildcardCoveringIt => {
            let name = domain_name(row);
            let (_, parent) = name.split_once('.').expect("a name of at least two labels");
            format!("*.{parent}")
        }
    }
}

/// The emulator with the three services this suite exercises, mounted on ONE engine
/// exactly as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`,
/// which is what routes the request and what each handler reads its Region from. The
/// signature is never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// What one call answered: its status, the `x-amzn-errortype` a refusal carries and its
/// body.
struct Answer {
    status: u16,
    error_type: String,
    text: String,
}

/// Dispatch one request and require that exactly one service claimed it.
async fn send(
    router: &MultiServiceRouter,
    case: &str,
    leg: &str,
    request: http::Request<Bytes>,
) -> Answer {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(['/', ':'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let text = String::from_utf8_lossy(response.body()).to_string();
    for refusal in ROUTER_REFUSALS {
        assert!(
            !text.contains(refusal) && !error_type.contains(refusal),
            "case {case:?}: {leg} must be served by exactly one service, got HTTP {status}: \
             {text}"
        );
    }
    Answer {
        status,
        error_type,
        text,
    }
}

/// Call one Certificate Manager operation over its awsJson1_1 wire, requiring HTTP 200.
async fn acm(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let host = format!("acm.{CERTIFICATE_REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(http::header::HOST, host)
        .header(http::header::CONTENT_TYPE, "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header(
            http::header::AUTHORIZATION,
            authorization("acm", CERTIFICATE_REGION),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let answered = send(router, case, &format!("ACM {operation}"), request).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: ACM {operation} expected HTTP 200, got {} with body {}",
        answered.status, answered.text
    );
    serde_json::from_str(&answered.text).unwrap_or(Value::Null)
}

/// Call one AppSync operation over its restJson1 wire, answering what it said.
async fn appsync(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, String, Value) {
    let payload = match &body {
        Some(document) => Bytes::from(document.to_string()),
        None => Bytes::new(),
    };
    let host = format!("appsync.{APPSYNC_REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header(http::header::HOST, host)
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(
            http::header::AUTHORIZATION,
            authorization("appsync", APPSYNC_REGION),
        )
        .body(payload)
        .expect("a well-formed request");
    let answered = send(router, case, &format!("AppSync {method} {path}"), request).await;
    let document: Value = serde_json::from_str(&answered.text).unwrap_or(Value::Null);
    (answered.status, answered.error_type, document)
}

// ---------------------------------------------------------------------------
// The tag-driven discovery, which every row's walk starts from
// ---------------------------------------------------------------------------

/// The most pages one walk over this account's zones can need: one per zone, plus the
/// final page. A walk that wants more is a marker that is not advancing.
fn page_limit() -> usize {
    DOMAIN_ROWS.len() + DECOY_ROWS.len() + 2
}

/// Every zone `ListHostedZones` enumerates at `page_size`, through the shared walk.
async fn all_zones(
    router: &MultiServiceRouter,
    case: &str,
    page_size: Option<i32>,
) -> Vec<ListedZone> {
    route53_zone_discovery::all_zones(router, case, page_size, page_limit()).await
}

/// The zones a tag-driven client selects, as `apex -> zone id`: the shared walk, selecting
/// on this suite's own [`SELECTOR`].
async fn selected_zones(
    router: &MultiServiceRouter,
    case: &str,
    page_size: Option<i32>,
) -> BTreeMap<String, String> {
    tag_selected_public_zones(router, case, page_size, page_limit(), SELECTOR).await
}

/// Create and tag every zone of the account, answering the bare id each apex was created
/// under.
async fn given_the_account_zones(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let mut ids = BTreeMap::new();
    for row in DOMAIN_ROWS {
        let id = create_zone(router, row.case, row.apex, false, row.apex).await;
        let mut tags = vec![SELECTOR];
        tags.extend_from_slice(row.extra_tags);
        tag_zone(router, row.case, &id, &tags).await;
        ids.insert(row.apex.to_string(), id);
    }
    for row in DECOY_ROWS {
        let id = create_zone(router, row.case, row.apex, row.private, row.apex).await;
        tag_zone(router, row.case, &id, row.tags).await;
        ids.insert(row.apex.to_string(), id);
    }
    ids
}

// ---------------------------------------------------------------------------
// The certificate the custom domain is served with
// ---------------------------------------------------------------------------

/// Whether `arn` has the form `CertificateArn` fixes for a certificate of this account in
/// `CERTIFICATE_REGION` (`@pattern` on `com.amazonaws.acm#Arn`, and AppSync's own
/// `certificateArn` pattern `^arn:[a-z-]*:(acm|iam):[a-z0-9-]*:\d{12}:certificate/…$`).
fn is_well_formed_certificate_arn(arn: &str) -> bool {
    let prefix = format!("arn:aws:acm:{CERTIFICATE_REGION}:{ACCOUNT}:certificate/");
    arn.strip_prefix(&prefix).is_some_and(|id| {
        !id.is_empty()
            && id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "_/-".contains(ch))
    })
}

/// Request one DNS-validated public certificate for `name` and answer the ARN ACM minted.
async fn request_certificate(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let answered = acm(
        router,
        case,
        "RequestCertificate",
        &json!({ "DomainName": name, "ValidationMethod": "DNS" }),
    )
    .await;
    let arn = answered["CertificateArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        is_well_formed_certificate_arn(&arn),
        "case {case:?}: expected a certificate ARN of this account in {CERTIFICATE_REGION}, \
         got {arn:?} in {answered}"
    );
    arn
}

/// The `CertificateDetail` ACM reports for `arn`.
async fn describe_certificate(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    acm(
        router,
        case,
        "DescribeCertificate",
        &json!({ "CertificateArn": arn }),
    )
    .await["Certificate"]
        .clone()
}

/// The challenge record ACM handed out for every name `arn` covers, as `(name, value)`
/// pairs — READ from ACM, never composed here.
async fn challenge_records(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
) -> Vec<(String, String)> {
    let detail = describe_certificate(router, case, arn).await;
    assert_eq!(
        detail["Status"].as_str(),
        Some(PENDING_VALIDATION),
        "case {case:?}: a name the account's own public zone is the authority for waits for \
         its proof, got {detail}"
    );
    let mut records = Vec::new();
    for validation in detail["DomainValidationOptions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let record = &validation["ResourceRecord"];
        let (Some(name), Some(kind), Some(value)) = (
            record["Name"].as_str(),
            record["Type"].as_str(),
            record["Value"].as_str(),
        ) else {
            panic!("case {case:?}: every pending DNS validation carries its record: {detail}");
        };
        assert_eq!(
            kind, CNAME,
            "case {case:?}: ACM's DNS challenge is a {CNAME}, got {kind:?} in {detail}"
        );
        records.push((name.to_string(), value.to_string()));
    }
    assert!(
        !records.is_empty(),
        "case {case:?}: a DNS-validated request carries a challenge per covered name, got \
         {detail}"
    );
    records
}

/// Publish one challenge record in the zone the discovery resolved, through the shared
/// publication.
async fn publish_challenge(
    router: &MultiServiceRouter,
    case: &str,
    zone_id: &str,
    name: &str,
    value: &str,
) {
    publish_record(
        router,
        case,
        zone_id,
        name,
        CNAME,
        CHALLENGE_TTL,
        value,
    )
    .await;
}

/// Poll `DescribeCertificate` until ACM reports `arn` issued, and answer that reading.
///
/// Waits for the CONDITION: the issuance is owed from the instant ACM first observes the
/// published proof, so a slower machine takes more polls and not a different answer.
async fn wait_for_issuance(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let polls = (DEADLINE.as_millis() / POLL.as_millis()) as usize;
    let mut detail = describe_certificate(router, case, arn).await;
    for _ in 0..polls {
        if detail["Status"].as_str() == Some(ISSUED) {
            return detail;
        }
        tokio::time::sleep(POLL).await;
        detail = describe_certificate(router, case, arn).await;
    }
    panic!(
        "case {case:?}: expected ACM to report {ISSUED} for the certificate whose challenge is \
         published, got {detail}"
    )
}

// ---------------------------------------------------------------------------
// The AppSync custom domain served on the discovered zone's name
// ---------------------------------------------------------------------------

/// `GetDomainName`'s path for `domain`.
fn domain_path(domain: &str) -> String {
    format!("/v1/domainnames/{domain}")
}

/// The path all three association operations share, for `domain`.
fn association_path(domain: &str) -> String {
    format!("/v1/domainnames/{domain}/apiassociation")
}

/// Require that `answered` is the modelled `NotFoundException` (404) stating `message`.
fn assert_not_found(case: &str, leg: &str, answered: &(u16, String, Value), message: &str) {
    let (status, error_type, document) = answered;
    assert_eq!(
        (*status, error_type.as_str(), document["message"].as_str()),
        (404, NOT_FOUND, Some(message)),
        "case {case:?}: {leg} must answer {NOT_FOUND} 404 stating {message:?}, got {status} \
         {error_type:?} {document}"
    );
}

/// A GraphQL API of the account's own, which a mapping names. Answers its `apiId`.
async fn given_graphql_api(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, _, answered) = appsync(
        router,
        case,
        "POST",
        "/v1/apis",
        Some(json!({ "name": name, "authenticationType": "API_KEY" })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CreateGraphqlApi, got {status} {answered}"
    );
    answered["graphqlApi"]["apiId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer an apiId: {answered}"))
        .to_string()
}

/// Poll `GetApiAssociation` until it reports `SUCCESS`, and answer that mapping.
async fn wait_for_association(router: &MultiServiceRouter, case: &str, domain: &str) -> Value {
    let polls = (DEADLINE.as_millis() / POLL.as_millis()) as usize;
    let mut answered = appsync(router, case, "GET", &association_path(domain), None).await;
    for _ in 0..polls {
        if answered.2["apiAssociation"]["associationStatus"].as_str() == Some(SUCCESS) {
            return answered.2["apiAssociation"].clone();
        }
        tokio::time::sleep(POLL).await;
        answered = appsync(router, case, "GET", &association_path(domain), None).await;
    }
    panic!(
        "case {case:?}: expected the deployed mapping to report {SUCCESS}, got {} {}",
        answered.0, answered.2
    )
}

/// Poll `GetApiAssociation` until it answers the not-found the unassociated domain earns,
/// and answer that reading.
async fn wait_for_no_association(
    router: &MultiServiceRouter,
    case: &str,
    domain: &str,
) -> (u16, String, Value) {
    let polls = (DEADLINE.as_millis() / POLL.as_millis()) as usize;
    let mut answered = appsync(router, case, "GET", &association_path(domain), None).await;
    for _ in 0..polls {
        if answered.0 == 404 {
            return answered;
        }
        tokio::time::sleep(POLL).await;
        answered = appsync(router, case, "GET", &association_path(domain), None).await;
    }
    panic!(
        "case {case:?}: expected the unmapped domain to carry no association, got {} {}",
        answered.0, answered.2
    )
}

#[tokio::test]
async fn every_tagged_public_zone_is_enumerated_and_tag_selected_at_every_page_size() {
    // The first half of the rule, on its own: the account's zones are enumerated across
    // every page size with the private flag each was created with, and each reports the
    // tags written on it — so the tag-driven selection resolves exactly the public zones
    // the selector tag was written on, and no private one however it is tagged.
    let router = build_router();
    let created = given_the_account_zones(&router).await;

    let expected_selection: BTreeMap<String, String> = DOMAIN_ROWS
        .iter()
        .map(|row| (row.apex.to_string(), created[row.apex].clone()))
        .collect();

    for page_size in page_sizes() {
        let case = format!("maxitems={page_size:?}");
        let listed = all_zones(&router, &case, page_size).await;
        assert_eq!(
            listed.len(),
            created.len(),
            "case {case:?}: the account holds {} zones, and the enumeration carried {}: \
             {listed:?}",
            created.len(),
            listed.len()
        );
        let walked: BTreeMap<String, bool> = listed
            .iter()
            .map(|zone| (zone.id.clone(), zone.private))
            .collect();
        assert_eq!(
            walked.len(),
            listed.len(),
            "case {case:?}: paging must enumerate each zone exactly once, got {listed:?}"
        );

        // Every zone, under the id its create minted, with the flag it was created with.
        for row in DECOY_ROWS {
            let id = &created[row.apex];
            assert_eq!(
                walked.get(id),
                Some(&row.private),
                "case {case:?}: the zone {id} created for {:?} must be enumerated reporting \
                 PrivateZone {}, got {listed:?}",
                row.case,
                row.private
            );
        }
        for row in DOMAIN_ROWS {
            let id = &created[row.apex];
            assert_eq!(
                walked.get(id),
                Some(&false),
                "case {case:?}: the public zone {id} created for {:?} must be enumerated \
                 reporting PrivateZone false, got {listed:?}",
                row.case
            );
            let mut expected: BTreeMap<String, String> = row
                .extra_tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            expected.insert(SELECTOR.0.to_string(), SELECTOR.1.to_string());
            let reported = zone_tags(&router, row.case, id).await;
            assert_eq!(
                reported, expected,
                "case {case:?} ({:?}): ListTagsForResource must report the tags written on the \
                 zone, got {reported:?}",
                row.case
            );
        }

        let selection = selected_zones(&router, &case, page_size).await;
        assert_eq!(
            selection, expected_selection,
            "case {case:?}: a client selecting PUBLIC zones by {SELECTOR:?} must resolve \
             exactly the public zones it was written on"
        );
    }
}

#[tokio::test]
async fn a_tag_discovered_zone_carries_an_appsync_custom_domain_through_its_lifecycle() {
    let router = build_router();
    let created = given_the_account_zones(&router).await;

    // The zone every row's walk starts from is the one the DISCOVERY resolved, not the one
    // the create happened to answer — that is the read this whole path is gated behind.
    let discovered = selected_zones(&router, "the discovery each walk starts from", Some(2)).await;
    assert_eq!(
        discovered.len(),
        DOMAIN_ROWS.len(),
        "the discovery must resolve one zone per row, got {discovered:?}"
    );

    // Request every row's certificate and publish every challenge first, so the issuance
    // each one is owed runs on one span rather than one per row.
    let mut requested: Vec<(&DomainRow, String, String)> = Vec::new();
    for row in DOMAIN_ROWS {
        let zone = discovered
            .get(row.apex)
            .unwrap_or_else(|| panic!("case {:?}: {} must be discovered", row.case, row.apex));
        assert_eq!(
            Some(zone),
            created.get(row.apex),
            "case {:?}: the discovered zone must be the one the create minted",
            row.case
        );
        let name = certificate_name(row);
        let arn = request_certificate(&router, row.case, &name).await;
        for (record, value) in challenge_records(&router, row.case, &arn).await {
            publish_challenge(&router, row.case, zone, &record, &value).await;
        }
        requested.push((row, name, arn));
    }

    for (row, name, arn) in &requested {
        let case = row.case;
        let issued = wait_for_issuance(&router, case, arn).await;
        assert_eq!(
            (
                issued["DomainName"].as_str(),
                issued["Type"].as_str(),
                issued["CertificateAuthorityArn"].is_null(),
            ),
            (Some(name.as_str()), Some("AMAZON_ISSUED"), true),
            "case {case:?}: the issued certificate is the public one requested for {name}, got \
             {issued}"
        );

        // The custom domain, served with that certificate on a name of the discovered
        // zone.
        let domain = domain_name(row);
        let (status, error_type, body) = appsync(
            &router,
            case,
            "POST",
            "/v1/domainnames",
            Some(json!({
                "domainName": domain,
                "certificateArn": arn,
                "description": format!("the custom domain of {}", row.apex),
            })),
        )
        .await;
        assert_eq!(
            (status, error_type.as_str()),
            (200, ""),
            "case {case:?}: expected CreateDomainName 200 for the certificate ACM issued for \
             {name}, got {status} {body}"
        );
        let config = body["domainNameConfig"].clone();
        assert_eq!(
            (
                config["domainName"].as_str(),
                config["certificateArn"].as_str(),
                config["hostedZoneId"].as_str(),
                config["domainNameArn"].as_str(),
            ),
            (
                Some(domain.as_str()),
                Some(arn.as_str()),
                Some(CLOUDFRONT_HOSTED_ZONE_ID),
                Some(
                    format!("arn:aws:appsync:{APPSYNC_REGION}:{ACCOUNT}:domainnames/{domain}")
                        .as_str()
                ),
            ),
            "case {case:?}: expected the stored configuration of {domain}, got {config}"
        );
        let endpoint = config["appsyncDomainName"].as_str().unwrap_or_default();
        assert!(
            endpoint
                .strip_suffix(APPSYNC_DOMAIN_SUFFIX)
                .is_some_and(|label| !label.is_empty()),
            "case {case:?}: expected an endpoint of a minted label plus \
             {APPSYNC_DOMAIN_SUFFIX:?}, got {endpoint:?}"
        );

        // The create answered from the store, so the read answers the same configuration.
        let (status, _, read) = appsync(&router, case, "GET", &domain_path(&domain), None).await;
        assert_eq!(
            (status, read["domainNameConfig"].clone()),
            (200, config.clone()),
            "case {case:?}: expected GetDomainName to report the created configuration, got \
             {status} {read}"
        );

        // A certificate ACM issued for ANOTHER name cannot serve this zone's names, and
        // the refused create stores nothing.
        let other = requested
            .iter()
            .find(|(candidate, _, _)| candidate.apex != row.apex)
            .map(|(_, _, other_arn)| other_arn.clone())
            .expect("the table holds more than one row");
        let mismatched = format!("mismatch.{}", row.apex);
        let (status, error_type, refused) = appsync(
            &router,
            case,
            "POST",
            "/v1/domainnames",
            Some(json!({ "domainName": mismatched, "certificateArn": other })),
        )
        .await;
        assert_eq!(
            (
                status,
                error_type.as_str(),
                refused["message"].as_str(),
            ),
            (400, BAD_REQUEST, Some(CERTIFICATE_INVALID)),
            "case {case:?}: a certificate issued for another name must be refused for \
             {mismatched}, got {status} {error_type:?} {refused}"
        );
        let (status, error_type, absent) =
            appsync(&router, case, "GET", &domain_path(&mismatched), None).await;
        assert_not_found(
            case,
            "GetDomainName after the refused create",
            &(status, error_type, absent),
            DOMAIN_NOT_FOUND_ON_READ,
        );

        // While the domain carries no API, the association read is the modelled 404 with
        // AppSync's own sentence for it.
        let before = appsync(&router, case, "GET", &association_path(&domain), None).await;
        assert_not_found(
            case,
            "GetApiAssociation before any AssociateApi",
            &before,
            ASSOCIATION_NOT_FOUND,
        );

        // One API mapped onto the domain: reported as being deployed, and successful once
        // the deployment the clock owes is due.
        let api_id =
            given_graphql_api(&router, case, &format!("api-for-{}", row.label.replace('.', "-")))
                .await;
        let (status, _, associated) = appsync(
            &router,
            case,
            "POST",
            &association_path(&domain),
            Some(json!({ "apiId": api_id })),
        )
        .await;
        assert_eq!(
            (
                status,
                associated["apiAssociation"]["domainName"].as_str(),
                associated["apiAssociation"]["apiId"].as_str(),
                associated["apiAssociation"]["associationStatus"].as_str(),
            ),
            (
                200,
                Some(domain.as_str()),
                Some(api_id.as_str()),
                Some(PROCESSING),
            ),
            "case {case:?}: expected a fresh mapping naming the domain and the API and being \
             deployed, got {status} {associated}"
        );
        let settled = wait_for_association(&router, case, &domain).await;
        assert_eq!(
            (
                settled["domainName"].as_str(),
                settled["apiId"].as_str(),
                settled["associationStatus"].as_str(),
            ),
            (
                Some(domain.as_str()),
                Some(api_id.as_str()),
                Some(SUCCESS),
            ),
            "case {case:?}: expected the deployed mapping to name its API, got {settled}"
        );

        // Unmapping leaves the domain carrying no association again, and the domain itself
        // exactly as it was created.
        let (status, _, disassociated) =
            appsync(&router, case, "DELETE", &association_path(&domain), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DisassociateApi 200, got {status} {disassociated}"
        );
        let after = wait_for_no_association(&router, case, &domain).await;
        assert_not_found(
            case,
            "GetApiAssociation after DisassociateApi",
            &after,
            ASSOCIATION_NOT_FOUND,
        );
        let (status, _, reread) = appsync(&router, case, "GET", &domain_path(&domain), None).await;
        assert_eq!(
            (status, reread["domainNameConfig"].clone()),
            (200, config),
            "case {case:?}: expected the domain itself to outlive its association, got {status} \
             {reread}"
        );

        // Deleting the domain releases the name: the read that follows is the modelled 404.
        let (status, _, deleted) =
            appsync(&router, case, "DELETE", &domain_path(&domain), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteDomainName 200, got {status} {deleted}"
        );
        let gone = appsync(&router, case, "GET", &domain_path(&domain), None).await;
        assert_not_found(
            case,
            "GetDomainName after DeleteDomainName",
            &gone,
            DOMAIN_NOT_FOUND_ON_READ,
        );
    }
}
