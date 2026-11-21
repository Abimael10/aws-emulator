// SPDX-License-Identifier: CC0-1.0

//! The representation of an API Gateway v2 CUSTOM DOMAIN NAME, and the Route 53 ALIAS a
//! caller writes from it.
//!
//! RULE: **the document `CreateDomainName` answers, and the one every later read of the same
//! resource reports, names the resource with an API Gateway management ARN whose account-id
//! section is EMPTY (`arn:aws:apigateway:{region}::/domainnames/{name}`, the Region and the
//! name taken from the request), and carries for every `DomainNameConfigurations` entry the
//! Route 53 hosted zone id AWS publishes for that KIND of endpoint — one per Region for a
//! regional endpoint, the one CloudFront zone for an edge-optimized one — beside an
//! `ApiGatewayDomainName` this emulator itself answers on; the create's `Tags` member is
//! present exactly when the request attached tags, while every reader reports the resource's
//! collection. So the alias record a caller writes from the published pair resolves to the
//! address the custom domain name is served at.**
//!
//! `DomainNameConfiguration$HostedZoneId` is "the Amazon Route 53 Hosted Zone ID of the
//! endpoint", `$ApiGatewayDomainName` is "a domain name for the API" — the value a caller
//! points its own DNS record at — and `AliasTarget$HostedZoneId` fixes the CloudFront zone
//! ("CloudFront distribution: Specify `Z2FDTNDATAQYW2`"). Two crates have to agree for the
//! last clause to hold (API Gateway v2 publishes the pair and serves the address, Route 53
//! resolves the alias written from it), so the suite lives here rather than in either of
//! them.
//!
//! The rows span what the rule generalises over: the Region the create is signed for, the
//! endpoint type of each entry (left to the default, named `REGIONAL`, `EDGE`), how many
//! entries one domain name carries, how deep the domain name itself is, and whether the
//! create attached tags. Every expected value is computed from the request and from the
//! published catalogs below, never from the answer being checked.
//!
//! The cross-Region leg of the read below asserts only the half of the rule that is about the
//! Region a resource was CREATED in: this emulator holds one account-wide collection of
//! custom domain names rather than one per Region (they are keyed by name in
//! `ComAmazonawsApigatewayv2Apigatewayv2State`), so which Regions a read is answered in at
//! all is not what this suite is about. What it pins is that the ARN and the published pair a
//! reader reports are the CREATING Region's and do not follow the Region the reader signed
//! for, exactly as the owning crate's `domain_name_lifecycle.rs`
//! (`a_custom_domain_name_is_named_by_an_account_less_api_gateway_arn`) already pins it.
//!
//! What the owning crate's own suites pin and this one does not restate: the per-Region
//! catalog in full and the refusals around a configuration list
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/custom_domain_hosted_zone.rs`,
//! `.../tests/all/domain_name_lifecycle.rs`). What is added here is the ALIAS leg: the pair
//! the representation publishes, written into a zone as Route 53 documents the association,
//! resolves to the address this emulator serves that domain name on.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// The XML element readers and the query-component encoder this test crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{element, elements, form_encode, opt_element};

/// The status `CreateDomainName` answers (`http POST /v2/domainnames code 201`), and the one
/// every read and every accepted change answers.
const CREATED: u16 = 201;
const OK: u16 = 200;

/// The hosted zone id AWS publishes for a CloudFront distribution, which is what fronts an
/// `EDGE` endpoint. SOURCE: `AliasTarget$HostedZoneId`, "CloudFront distribution: Specify
/// Z2FDTNDATAQYW2".
const CLOUDFRONT_ZONE: &str = "Z2FDTNDATAQYW2";

/// The hosted zone id AWS publishes for API Gateway's REGIONAL endpoint, Region by Region.
///
/// SOURCE: the table `DomainNameConfiguration$HostedZoneId` points at (API Gateway's
/// published Regions-and-endpoints list for a regional custom domain name). AWS ASSIGNS
/// these ids and publishes no formula for them, so this suite carries the ids it publishes
/// for the Regions the rows below are created in — `us-west-2` among them, beside two
/// Regions no row of the recorded case names.
const REGIONAL_ZONES: &[(&str, &str)] = &[
    ("us-west-2", "Z2OJLYMUO9EFXC"),
    ("eu-west-1", "ZLY8HYME6SFDD"),
    ("ap-northeast-1", "Z1YSHQZHG15GKL"),
];

/// The `EndpointType` of an entry that is fronted by CloudFront, and the one every entry
/// that names no type is left at (`DomainNameConfiguration$EndpointType` documents `REGIONAL`
/// as the kind "for a regional API and its custom domain name").
const EDGE: &str = "EDGE";
const REGIONAL: &str = "REGIONAL";

/// The account id every ARN this emulator mints carries (`engine::EMULATOR_ACCOUNT_ID`). An
/// API Gateway management ARN carries NO account section at all, so this value appearing in
/// one is the defect.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The status and sentence the custom domain name's own front door turns away a request that
/// no api mapping of the domain answers — which is every request here, because these rows map
/// no api at all. It is what tells an address THIS emulator serves from one it does not.
const NO_MAPPING: u16 = 403;
const NO_MAPPING_MESSAGE: &str = "Forbidden";

/// The `ChangeInfo$Status` a Route 53 change batch reaches once every name server has it, and
/// the response code of a DNS query the zone answers.
const INSYNC: &str = "INSYNC";
const NOERROR: &str = "NOERROR";

/// How long one dial and one exchange on the published address may take. All of it is in this
/// process; a case that cannot finish inside it has found a defect, not a slow machine.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the suite keeps asking whether a clock-owed transition has come due, and how long
/// it waits between looks. The wait is on the CONDITION; the deadline is only the point past
/// which the suite stops looking, and it is far above anything a loaded host needs.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` and `region`, which
/// is what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0"
    )
}

/// Call one API Gateway v2 operation over its restJson1 wire, signed for `region`.
async fn apigatewayv2(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(document) => Bytes::from(document.to_string()),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("apigateway.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway", region))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Call one Amazon Route 53 operation over its restXml wire.
async fn route53(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("route53.{region}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("route53", region))
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// A string member of an answered document, or a panic quoting the whole document.
fn member(case: &str, document: &Value, name: &str) -> String {
    document
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} in {document}"))
        .to_string()
}

/// The `domainNameConfigurations` entries of an answered document, which every row carries at
/// least one of.
fn configurations(case: &str, document: &Value) -> Vec<Value> {
    let entries = document
        .get("domainNameConfigurations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        !entries.is_empty(),
        "case {case:?}: expected the domain name's configurations, got {document}"
    );
    entries
}

/// The `(hostedZoneId, apiGatewayDomainName)` pair of each configuration entry, in the order
/// the document lists them: the whole of what a caller needs to write the alias record.
fn alias_pairs(case: &str, document: &Value) -> Vec<(String, String)> {
    configurations(case, document)
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let case = format!("{case} (configuration {index})");
            (
                member(&case, entry, "hostedZoneId"),
                member(&case, entry, "apiGatewayDomainName"),
            )
        })
        .collect()
}

/// The ARN a custom domain name named `domain` and created in `region` must report: the API
/// Gateway management form, whose account-id section is EMPTY. Written out from the request's
/// own values rather than built through the emulator's ARN helper, which always writes an
/// account in.
fn expected_arn(region: &str, domain: &str) -> String {
    format!("arn:aws:apigateway:{region}::/domainnames/{domain}")
}

/// One row of the table: the create to make, and what its representation must report.
struct Row {
    case: &'static str,
    region: &'static str,
    domain: &'static str,
    /// The `domainNameConfigurations` the create asks for.
    configurations: Value,
    /// The `EndpointType` each resulting entry describes, in order.
    endpoint_types: Vec<&'static str>,
    /// The `tags` member of the request, or `None` for a create that attaches none.
    tags: Option<Value>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one configuration left to the default endpoint type, no tags",
            region: "us-west-2",
            domain: "v2-representation.example.com",
            configurations: json!([{}]),
            endpoint_types: vec![REGIONAL],
            tags: None,
        },
        Row {
            case: "a named regional endpoint with a certificate, in another Region, one tag",
            region: "eu-west-1",
            domain: "shop.api.v2-representation.example.com",
            configurations: json!([{
                "endpointType": REGIONAL,
                "certificateArn":
                    "arn:aws:acm:eu-west-1:000000000000:certificate/\
                     11111111-2222-3333-4444-555555555555"
            }]),
            endpoint_types: vec![REGIONAL],
            tags: Some(json!({ "team": "edge" })),
        },
        Row {
            case: "an edge-optimized endpoint, no tags",
            region: "ap-northeast-1",
            domain: "v2-edge.example.com",
            configurations: json!([{ "endpointType": EDGE }]),
            endpoint_types: vec![EDGE],
            tags: None,
        },
        Row {
            case: "two entries of different types on a deep name, two tags",
            region: "us-west-2",
            domain: "api.eu.v2-both-types.example.com",
            configurations: json!([
                { "endpointType": EDGE },
                { "endpointType": REGIONAL }
            ]),
            endpoint_types: vec![EDGE, REGIONAL],
            tags: Some(json!({ "team": "edge", "env": "test" })),
        },
    ]
}

/// The hosted zone id an entry of `endpoint_type` created in `region` must report: the
/// published CloudFront zone for an edge-optimized endpoint, and the Region's own published
/// zone for a regional one.
fn expected_zone(region: &str, endpoint_type: &str) -> String {
    if endpoint_type == EDGE {
        return CLOUDFRONT_ZONE.to_string();
    }
    REGIONAL_ZONES
        .iter()
        .find(|(zone_region, _)| *zone_region == region)
        .map(|(_, zone)| (*zone).to_string())
        .unwrap_or_else(|| panic!("this suite carries no published zone id for {region:?}"))
}

/// Create the domain name one row describes and answer the created document.
async fn create(router: &MultiServiceRouter, row: &Row) -> Value {
    let mut request = json!({
        "domainName": row.domain,
        "domainNameConfigurations": row.configurations.clone()
    });
    if let Some(tags) = &row.tags {
        request["tags"] = tags.clone();
    }
    let (status, created) =
        apigatewayv2(router, row.region, "POST", "/v2/domainnames", Some(request)).await;
    assert_eq!(
        status, CREATED,
        "case {:?}: expected {CREATED} from CreateDomainName, got {status} with {created}",
        row.case
    );
    created
}

/// Read the domain name back through `GetDomainName`, signed for `region`.
async fn read(router: &MultiServiceRouter, case: &str, region: &str, domain: &str) -> Value {
    let (status, document) = apigatewayv2(
        router,
        region,
        "GET",
        &format!("/v2/domainnames/{domain}"),
        None,
    )
    .await;
    assert_eq!(
        status, OK,
        "case {case:?}: expected {OK} from GetDomainName, got {status} with {document}"
    );
    document
}

/// What a client that DIALS the published address is answered: the whole status line and
/// headers, and the body.
async fn exchange(case: &str, published: &str, host: &str) -> (u16, String) {
    let authority: std::net::SocketAddr = published.parse().unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected apiGatewayDomainName to be an address a client can \
             dial, got {published:?}: {error}"
        )
    });
    let connecting = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, authority.port()));
    let mut stream = tokio::time::timeout(EXCHANGE_TIMEOUT, connecting)
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: {published} did not accept a connection"))
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {published} refused a connection: {error}")
        });
    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.write_all(request.as_bytes()))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the request was not written in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not written: {error}"));
    let mut raw = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut raw))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: {published} did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    let answered = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = answered
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("case {case:?}: expected an HTTP answer, got {answered:?}"));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a status line, got {head:?}"));
    (status, body.to_string())
}

/// Every row's created representation, and the one every read of it reports: the account-less
/// ARN, the published zone of each entry, the one address the domain name is served on, and
/// the tags member the create was asked for.
#[tokio::test(flavor = "multi_thread")]
async fn the_created_custom_domain_representation_is_the_one_every_read_reports() {
    let router = build_router();

    for row in rows() {
        let case = row.case;
        let created = create(&router, &row).await;

        // 1. The resource's own name: the API Gateway management ARN, whose account-id
        //    section is empty, for the Region the create was signed for and the name it
        //    asked for.
        let arn = member(case, &created, "domainNameArn");
        let expected = expected_arn(row.region, row.domain);
        assert_eq!(
            arn, expected,
            "case {case:?}: expected CreateDomainName to report {expected:?}, got {created}"
        );
        let sections: Vec<&str> = arn.splitn(6, ':').collect();
        assert_eq!(
            sections.len(),
            6,
            "case {case:?}: expected the six sections of an ARN, got {arn:?}"
        );
        assert_eq!(
            sections[4], "",
            "case {case:?}: an API Gateway management ARN carries no account-id section, got \
             {:?} in {arn:?}",
            sections[4]
        );
        assert!(
            !arn.contains(ACCOUNT),
            "case {case:?}: expected no account id in {arn:?}, which carries {ACCOUNT}"
        );

        // 2. The tags member: the create answers the collection it was ASKED to attach, and
        //    no member at all for a request that attached none — the presence is what tells
        //    a tagged resource from an untagged one.
        assert_eq!(
            created.get("tags"),
            row.tags.as_ref(),
            "case {case:?}: expected CreateDomainName to answer the tags it attached ({:?}), \
             got {created}",
            row.tags
        );

        // 3. Each entry's alias pair: the published zone of the KIND of endpoint it
        //    describes, beside the one address this domain name is served on.
        let pairs = alias_pairs(case, &created);
        assert_eq!(
            pairs.len(),
            row.endpoint_types.len(),
            "case {case:?}: expected {} configurations, got {created}",
            row.endpoint_types.len()
        );
        let entries = configurations(case, &created);
        for (index, endpoint_type) in row.endpoint_types.iter().enumerate() {
            let entry = &entries[index];
            assert_eq!(
                member(case, entry, "endpointType"),
                *endpoint_type,
                "case {case:?}: expected configuration {index} to describe a \
                 {endpoint_type} endpoint, got {entry}"
            );
            let zone = expected_zone(row.region, endpoint_type);
            assert_eq!(
                pairs[index].0, zone,
                "case {case:?}: expected configuration {index} ({endpoint_type} in {}) to \
                 report the published zone {zone:?}, got {entry}",
                row.region
            );
            assert_eq!(
                pairs[index].1, pairs[0].1,
                "case {case:?}: every configuration of one domain name is served on its one \
                 address, got {entry}"
            );
        }

        // 4. The address is one THIS emulator answers for THIS domain name: a request made
        //    there for the domain's own Host reaches the custom domain's front door, which
        //    turns it away because no api mapping of the domain answers the path.
        let published = pairs[0].1.clone();
        let (status, body) = exchange(case, &published, row.domain).await;
        assert_eq!(
            status, NO_MAPPING,
            "case {case:?}: expected the front door at {published} to answer {NO_MAPPING} \
             for a domain with no api mapping, got {status} with {body:?}"
        );
        let answered: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        assert_eq!(
            answered.get("message").and_then(Value::as_str),
            Some(NO_MAPPING_MESSAGE),
            "case {case:?}: expected the gateway's own refusal, got {body:?}"
        );

        // 5. And the read reports the same representation — the same ARN and the same pairs,
        //    signed for the creating Region and for another one, because an ARN names the
        //    Region its resource was created in. `tags` is the RESOURCE's collection on a
        //    read, which is the empty map for a resource nothing tagged.
        let collection = row.tags.clone().unwrap_or_else(|| json!({}));
        for reader_region in [row.region, "us-east-2"] {
            let document = read(&router, case, reader_region, row.domain).await;
            assert_eq!(
                member(case, &document, "domainNameArn"),
                expected,
                "case {case:?} (read in {reader_region:?}): expected the ARN the create \
                 answered, got {document}"
            );
            assert_eq!(
                alias_pairs(case, &document),
                pairs,
                "case {case:?} (read in {reader_region:?}): expected the pairs the create \
                 answered, got {document}"
            );
            assert_eq!(
                document.get("tags"),
                Some(&collection),
                "case {case:?} (read in {reader_region:?}): expected the resource's \
                 collection ({collection}), got {document}"
            );
        }
    }
}

/// The alias leg: the pair the representation publishes, written into a zone as Route 53
/// documents the association, resolves to the address this emulator serves the domain name
/// on — for both kinds of endpoint zone.
#[tokio::test(flavor = "multi_thread")]
async fn an_alias_written_from_the_published_pair_resolves_to_the_served_address() {
    let router = build_router();
    let aliased: Vec<Row> = rows()
        .into_iter()
        .filter(|row| row.region == "us-west-2")
        .collect();
    assert_eq!(
        aliased.len(),
        2,
        "expected the rows aliased here to cover a regional and an edge-fronted entry"
    );

    for row in aliased {
        let case = row.case;
        let created = create(&router, &row).await;
        let (hosted_zone_id, published) = alias_pairs(case, &created)[0].clone();
        let authority: std::net::SocketAddr = published.parse().unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected apiGatewayDomainName to be an address a client can \
                 dial, got {published:?}: {error}"
            )
        });

        // The zone the caller owns, and the ALIAS record set that points the custom domain
        // name at what API Gateway published — exactly the record AWS documents for this
        // association.
        let zone_name = format!("alias-{}", row.domain);
        let (status, zone) = route53(
            &router,
            row.region,
            "POST",
            "/2013-04-01/hostedzone",
            &format!(
                "<CreateHostedZoneRequest \
                 xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\"><Name>{zone_name}\
                 </Name><CallerReference>{}</CallerReference></CreateHostedZoneRequest>",
                row.domain
            ),
        )
        .await;
        assert_eq!(
            status, CREATED,
            "case {case:?}: expected {CREATED} from CreateHostedZone, got {status} with {zone}"
        );
        let zone_id = element(&zone, "Id")
            .trim_start_matches("/hostedzone/")
            .to_string();
        let record_name = format!("api.{zone_name}");

        let (status, changed) = route53(
            &router,
            row.region,
            "POST",
            &format!("/2013-04-01/hostedzone/{zone_id}/rrset"),
            &format!(
                "<ChangeResourceRecordSetsRequest \
                 xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\"><ChangeBatch>\
                 <Changes><Change><Action>CREATE</Action><ResourceRecordSet>\
                 <Name>{record_name}</Name><Type>A</Type><AliasTarget>\
                 <HostedZoneId>{hosted_zone_id}</HostedZoneId><DNSName>{published}</DNSName>\
                 <EvaluateTargetHealth>false</EvaluateTargetHealth></AliasTarget>\
                 </ResourceRecordSet></Change></Changes></ChangeBatch>\
                 </ChangeResourceRecordSetsRequest>"
            ),
        )
        .await;
        assert_eq!(
            status, OK,
            "case {case:?}: expected {OK} from ChangeResourceRecordSets, got {status} with \
             {changed}"
        );
        let change_id = element(&changed, "Id")
            .trim_start_matches("/change/")
            .to_string();

        // The change propagates on a span Route 53 owes; the suite looks again until it has,
        // and the deadline is only where it stops looking.
        let mut change_status = element(&changed, "Status");
        let deadline = Instant::now() + SETTLE_DEADLINE;
        while change_status != INSYNC && Instant::now() < deadline {
            tokio::time::sleep(LOOK_INTERVAL).await;
            let (status, document) = route53(
                &router,
                row.region,
                "GET",
                &format!("/2013-04-01/change/{change_id}"),
                "",
            )
            .await;
            assert_eq!(
                status, OK,
                "case {case:?}: expected {OK} from GetChange, got {status} with {document}"
            );
            change_status = element(&document, "Status");
        }
        assert_eq!(
            change_status, INSYNC,
            "case {case:?}: expected the change to reach {INSYNC}, got {change_status:?}"
        );

        // What a resolver gets for the aliased name: the ADDRESS the front door answers on.
        let (status, answered) = route53(
            &router,
            row.region,
            "GET",
            &format!(
                "/2013-04-01/testdnsanswer?hostedzoneid={zone_id}&recordname={}&recordtype=A",
                form_encode(&record_name)
            ),
            "",
        )
        .await;
        assert_eq!(
            status, OK,
            "case {case:?}: expected {OK} from TestDNSAnswer, got {status} with {answered}"
        );
        assert_eq!(
            opt_element(&answered, "ResponseCode").as_deref(),
            Some(NOERROR),
            "case {case:?}: expected {NOERROR}, got {answered}"
        );
        let resolved = elements(&answered, "RecordDataEntry");
        assert_eq!(
            resolved,
            vec![authority.ip().to_string()],
            "case {case:?}: expected the address the front door answers on ({}), got \
             {resolved:?}",
            authority.ip()
        );

        // And the resolved address really is that front door: a request made there for the
        // custom domain name is answered by the gateway itself. (The port comes from the
        // control plane, which is where a client on this machine reads it: DNS carries the
        // address alone, and the core binds its listeners on the loopback interface.)
        assert_eq!(
            resolved[0],
            Ipv4Addr::LOCALHOST.to_string(),
            "case {case:?}: expected the resolved address to be the loopback interface the \
             core binds its listeners on, got {:?}",
            resolved[0]
        );
        let (status, body) = exchange(case, &published, row.domain).await;
        assert_eq!(
            status, NO_MAPPING,
            "case {case:?}: expected the front door the alias resolves to to answer \
             {NO_MAPPING}, got {status} with {body:?}"
        );
    }
}
