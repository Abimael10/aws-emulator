// SPDX-License-Identifier: CC0-1.0

//! The GEOGRAPHIC RESTRICTION at the address a deployed distribution PUBLISHED, decided
//! against the same viewer country the edge reports to the origin.
//!
//! RULE: a distribution's `Restrictions.GeoRestriction` decides whether the viewer's country
//! is served at all — `blacklist` names the countries CloudFront does not distribute in, so a
//! viewer in one of them is refused by CloudFront itself and every other viewer is served;
//! `whitelist` names the countries it does, so a viewer anywhere else is refused — and the
//! country the decision is made against is the ONE country that request has, which is the
//! value the edge also reports to the origin as `CloudFront-Viewer-Country`. So a caller that
//! reads the country off the origin's report and then names it in a `blacklist` is refused,
//! and one that names it in a `whitelist` is served.
//!
//! Every row drives that rule where a viewer meets it: a connection to the `host:port` the
//! distribution published, with no AWS credential, read off the wire.
//!
//! The origin is the Lambda function URL that REPORTS the request it was handed, and the
//! topology, the dial and the reading of one answer are [`crate::edge_lambda_url_origin`]'s,
//! reused rather than copied: its default behaviour already carries the managed origin request
//! policy that forwards the `CloudFront-*` headers, without which no origin sees the country.

use crate::edge_lambda_url_origin::{
    Answered, Topology, ALL_VIEWER_AND_CLOUDFRONT, MISS, OPEN_PATH, ORIGIN_MARKER,
};
use crate::edge_signed_access::CACHING_DISABLED;
use crate::distribution_lifecycle::API_VERSION;

/// `X-Cache` for an answer CloudFront produced itself, which is what a refused viewer reads.
const ERROR: &str = "Error from cloudfront";

/// How long the pooled client below waits for one answer.
///
/// Deliberately far past what an answer needs: the address is a listener of this process, and
/// what answers there runs a function on a machine that may be busy with every other test of
/// this workspace. What a row asserts is the ANSWER, never how quickly it came, so this exists
/// only to fail a request nothing answered rather than to time one.
const POOLED_CLIENT_DEADLINE: ::std::time::Duration = ::std::time::Duration::from_secs(60);

/// Two-letter uppercase country codes `GeoRestriction$Items` takes ("the `Location` element is
/// a two-letter, uppercase country code for a country that you want to include in your
/// `blacklist` or `whitelist`"), neither of them the country this edge reports for a viewer
/// that states none: New Zealand and Brazil, as ISO 3166-1 alpha-2 assigns them.
const ELSEWHERE: [&str; 2] = ["NZ", "BR"];

/// How a row names the countries of its restriction, relative to the country the EDGE itself
/// reports for the viewer under test.
#[derive(Debug)]
enum Locations {
    /// No `Restrictions` element at all.
    None,
    /// The country the edge reports for this viewer, alone.
    TheViewers,
    /// The country the edge reports, among others.
    TheViewersAmongOthers,
    /// Countries the edge does not report for this viewer.
    Elsewhere,
}

/// One row: the restriction a distribution carries, and whether a viewer of this emulator is
/// refused under it.
struct GeoRow {
    case: &'static str,
    /// `GeoRestriction$RestrictionType`, or the empty string for a configuration that carries
    /// no `Restrictions` at all.
    restriction_type: &'static str,
    locations: Locations,
    refused: bool,
}

/// The rows: the restriction type (`none`, `blacklist`, `whitelist`, and a configuration
/// carrying no restriction element), how many locations it names, and whether the country the
/// edge reports for the viewer is among them.
const GEO_ROWS: &[GeoRow] = &[
    GeoRow {
        case: "a configuration carrying no restriction at all",
        restriction_type: "",
        locations: Locations::None,
        refused: false,
    },
    GeoRow {
        case: "a restriction type that restricts nobody",
        restriction_type: "none",
        locations: Locations::None,
        refused: false,
    },
    GeoRow {
        case: "a blocklist naming the country the edge reports for this viewer",
        restriction_type: "blacklist",
        locations: Locations::TheViewers,
        refused: true,
    },
    GeoRow {
        case: "a blocklist naming that country among others",
        restriction_type: "blacklist",
        locations: Locations::TheViewersAmongOthers,
        refused: true,
    },
    GeoRow {
        case: "a blocklist naming only other countries",
        restriction_type: "blacklist",
        locations: Locations::Elsewhere,
        refused: false,
    },
    GeoRow {
        case: "an allowlist naming the country the edge reports for this viewer",
        restriction_type: "whitelist",
        locations: Locations::TheViewers,
        refused: false,
    },
    GeoRow {
        case: "an allowlist naming only other countries",
        restriction_type: "whitelist",
        locations: Locations::Elsewhere,
        refused: true,
    },
];

/// The `Restrictions` element naming `locations` under `restriction_type`, or the empty string
/// for a configuration that carries none.
fn restrictions(restriction_type: &str, locations: &[String]) -> String {
    if restriction_type.is_empty() {
        return String::new();
    }
    let items: String = locations
        .iter()
        .map(|location| format!("<Location>{location}</Location>"))
        .collect();
    let items = if items.is_empty() {
        String::new()
    } else {
        format!("<Items>{items}</Items>")
    };
    format!(
        "<Restrictions><GeoRestriction>\
         <RestrictionType>{restriction_type}</RestrictionType>\
         <Quantity>{}</Quantity>{items}\
         </GeoRestriction></Restrictions>",
        locations.len()
    )
}

/// One `DistributionConfig` over the function URL `origin` carrying `restrictions`: one
/// `GET`/`HEAD` behaviour, the managed `CachingDisabled` cache policy — so every row's request
/// is an origin fetch rather than another row's cached answer — and the managed origin request
/// policy that forwards the `CloudFront-*` headers the edge generates.
fn config_xml(
    caller_reference: &str,
    origin: &str,
    origin_extra: &str,
    restrictions: &str,
) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>function-url</Id><DomainName>{origin}</DomainName>{origin_extra}\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         <OriginSslProtocols><Quantity>1</Quantity>\
         <Items><SslProtocol>TLSv1.2</SslProtocol></Items></OriginSslProtocols>\
         </CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>function-url</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         <OriginRequestPolicyId>{ALL_VIEWER_AND_CLOUDFRONT}</OriginRequestPolicyId>\
         </DefaultCacheBehavior>\
         <Comment>a geo-restricted function url behind a distribution</Comment>\
         {restrictions}\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// The country the edge reported to the origin, off the origin's own report of the request it
/// served — held to being the two-letter uppercase code the header carries.
fn reported_country(case: &str, answered: &Answered) -> String {
    let report = answered.report(case);
    let country = report["country"].as_str().unwrap_or_default().to_string();
    assert!(
        country.len() == 2 && country.chars().all(|letter| letter.is_ascii_uppercase()),
        "case {case:?}: expected the origin to be told a two-letter uppercase country code, \
         got {country:?} in {report}"
    );
    country
}

/// RULE, over the origin whose fetch the edge SIGNS: the geographic restriction and the
/// generated headers are the edge's own work whatever credential its origin request carries,
/// so a distribution whose origin is signed by an `OriginAccessControl` refuses the viewer its
/// restriction names and serves every other viewer the origin's answer — with the country it
/// decided against reported to that origin.
///
/// Driven on an emulator that VERIFIES every signature and is SERVED at an endpoint, as a
/// started one is: the fetch the edge makes is a signed in-process call, and the origin request
/// policy is the one that forwards the `CloudFront-*` headers beside every viewer header, so
/// the signature the edge computes covers the request it really sends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_geo_restriction_decides_a_viewer_of_an_oac_signed_origin() {
    let topology = Topology::verifying();
    ::emulator_server::EmulatorService::serving_at(&topology.router, "https://127.0.0.1:9297");
    let setup = "a geo-restricted distribution over an OAC-signed function URL origin";
    let function = "cf-edge-geo-oac";
    let announced = topology
        .function_url_host(
            setup,
            function,
            aws_sdk_lambda::types::FunctionUrlAuthType::AwsIam,
        )
        .await;
    // What an `Origin$DomainName` carries: the authority of the published endpoint.
    let origin = announced.split('/').next().unwrap_or_default().to_string();
    let control = topology
        .origin_access_control(setup, "edge-geo-oac")
        .await;
    let origin_extra = format!("<OriginAccessControlId>{control}</OriginAccessControlId>");

    // A blocklist naming countries this viewer is not in: the viewer is served, and the
    // country the edge reports is read off the origin's own report of the request.
    let elsewhere: Vec<String> = ELSEWHERE.iter().map(|code| (*code).to_string()).collect();
    let served_case = "a viewer outside the blocklist of an OAC-signed origin";
    let (id, arn, domain) = topology
        .created_distribution(
            served_case,
            &config_xml(
                "edge-geo-oac-served",
                &origin,
                &origin_extra,
                &restrictions("blacklist", &elsewhere),
            ),
        )
        .await;
    // The grant that makes the signed fetch one Lambda admits FOR this distribution.
    for action in ["lambda:InvokeFunctionUrl", "lambda:InvokeFunction"] {
        topology
            .lambda()
            .add_permission()
            .function_name(function)
            .statement_id(action.replace(':', "-"))
            .action(action)
            .principal("cloudfront.amazonaws.com")
            .source_arn(&arn)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup {setup}: granting {action}: {error}"));
    }
    topology.awaited_deployment(served_case, &id).await;
    let answered = Answered::of_stating(served_case, &domain, "GET", OPEN_PATH, &[]).await;
    assert_eq!(
        answered.status, 200,
        "case {served_case:?}: expected the signed origin's answer through the edge, got {} \
         with {:?}",
        answered.status, answered.raw
    );
    assert!(
        answered.carries_the_origins_bytes(),
        "case {served_case:?}: expected the origin's own bytes, got {:?}",
        answered.body
    );
    let viewers_country = reported_country(served_case, &answered);

    // The same topology, with the country the edge reports for this viewer blocklisted: the
    // edge refuses it itself and makes no origin request, signed or not.
    let refused_case = "a viewer the blocklist of an OAC-signed origin names";
    let (id, arn, domain) = topology
        .created_distribution(
            refused_case,
            &config_xml(
                "edge-geo-oac-refused",
                &origin,
                &origin_extra,
                &restrictions("blacklist", std::slice::from_ref(&viewers_country)),
            ),
        )
        .await;
    for action in ["lambda:InvokeFunctionUrl", "lambda:InvokeFunction"] {
        topology
            .lambda()
            .add_permission()
            .function_name(function)
            .statement_id(format!("{}-refused", action.replace(':', "-")))
            .action(action)
            .principal("cloudfront.amazonaws.com")
            .source_arn(&arn)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup {setup}: granting {action}: {error}"));
    }
    topology.awaited_deployment(refused_case, &id).await;
    let refused = Answered::of_stating(refused_case, &domain, "GET", OPEN_PATH, &[]).await;
    assert_eq!(
        refused.status, 403,
        "case {refused_case:?} ({viewers_country:?}): expected CloudFront's own 403, got {} \
         with {:?}",
        refused.status, refused.raw
    );
    assert!(
        refused.reports_cache(ERROR) && !refused.carries_the_origins_bytes(),
        "case {refused_case:?}: expected CloudFront's own answer ({ERROR:?}) carrying none of \
         the origin's bytes, got {:?}",
        refused.raw
    );
}

/// RULE, over the viewer that asks more than once: the edge answers EVERY viewer request on a
/// connection, so a client that keeps one connection open and asks again on it is answered
/// again — a refused viewer and a served one alike, whichever order they come in.
///
/// A client pooling its connections is how a stock HTTP client asks twice, so the rows are
/// driven through ONE such client: what each one asserts is the answer it got, never how
/// quickly it came, and the client's own deadline exists only to fail a request nothing
/// answered rather than to time one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_request_of_one_pooled_client_is_answered_at_the_published_address() {
    let topology = Topology::new();
    let setup = "a viewer that keeps its connection open";
    let function = "cf-edge-geo-pooled";
    let origin = topology.public_function_url_host(setup, function).await;
    // A blocklist naming countries this viewer is not in, so the SAME address answers a served
    // viewer and a refused one: the refused rows below state one of those countries, which is
    // how a viewer of a loopback emulator says where it is.
    let elsewhere: Vec<String> = ELSEWHERE.iter().map(|code| (*code).to_string()).collect();
    let (id, _arn, domain) = topology
        .created_distribution(
            setup,
            &config_xml(
                "edge-geo-published-pooled",
                &origin,
                "",
                &restrictions("blacklist", &elsewhere),
            ),
        )
        .await;
    topology.awaited_deployment(setup, &id).await;

    // The rows: whether the request states a blocklisted country, interleaved so a served
    // answer follows a refused one and the other way round — each one on the connection the
    // one before it left open.
    // (case, the country the viewer states, the status it must be answered)
    let rows: [(&str, Option<&str>, u16); 5] = [
        ("the first request of the client", None, 200),
        ("a refused request on the same connection", Some(ELSEWHERE[0]), 403),
        ("a served request after a refusal", None, 200),
        ("the other blocklisted country", Some(ELSEWHERE[1]), 403),
        ("the client's last request", None, 200),
    ];

    let url = format!("http://{domain}{OPEN_PATH}");
    let answered = ::tokio::task::spawn_blocking(move || {
        // ONE client for every row, so each request after the first is asked on the
        // connection the client is holding.
        let client = ::reqwest::blocking::Client::builder()
            .timeout(POOLED_CLIENT_DEADLINE)
            .build()
            .expect("an HTTP client");
        rows.iter()
            .map(|(case, country, _expected)| {
                let mut request = client.get(&url);
                if let Some(country) = country {
                    request = request.header("cloudfront-viewer-country", *country);
                }
                let answered = request.send().unwrap_or_else(|error| {
                    panic!("case {case:?}: the published address did not answer {url}: {error}")
                });
                let status = answered.status().as_u16();
                (status, answered.text().unwrap_or_default())
            })
            .collect::<Vec<(u16, String)>>()
    })
    .await
    .unwrap_or_else(|error| panic!("setup {setup:?}: the client did not finish: {error}"));

    for ((case, country, expected), (status, body)) in rows.iter().zip(answered.iter()) {
        assert_eq!(
            status, expected,
            "case {case:?} (stating {country:?}): expected {expected}, got {status} with \
             {body:?}"
        );
        if *expected == 200 {
            assert!(
                body.contains(ORIGIN_MARKER),
                "case {case:?}: expected the origin's own bytes, got {body:?}"
            );
            continue;
        }
        assert!(
            !body.contains(ORIGIN_MARKER),
            "case {case:?}: a refused viewer must be served none of the origin's bytes, got \
             {body:?}"
        );
    }
}

/// RULE: the geographic restriction is enforced at the address the distribution published,
/// against the same country the edge reports to the origin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_geo_restriction_decides_a_viewer_at_the_published_address() {
    let topology = Topology::new();
    let setup = "a function URL origin behind a geo-restricted distribution";
    let function = "cf-edge-geo-published";
    let origin = topology.public_function_url_host(setup, function).await;

    // The country the EDGE reports for a viewer of this emulator, read where a client reads
    // it: off the origin's report of a request that was served. Every row below names its
    // locations relative to this one value, exactly as a caller that blocklists "the country
    // my requests come from" does.
    let (id, _arn, domain) = topology
        .created_distribution(setup, &config_xml("edge-geo-published-base", &origin, "", ""))
        .await;
    topology.awaited_deployment(setup, &id).await;
    let baseline = Answered::of_stating(setup, &domain, "GET", OPEN_PATH, &[]).await;
    assert_eq!(
        baseline.status, 200,
        "setup {setup:?}: expected the origin's answer through the edge, got {} with {:?}",
        baseline.status, baseline.raw
    );
    let viewers_country = reported_country(setup, &baseline);
    assert!(
        !ELSEWHERE.contains(&viewers_country.as_str()),
        "setup {setup:?}: the countries this suite names as elsewhere ({ELSEWHERE:?}) must not \
         be the one the edge reports ({viewers_country:?})"
    );

    for (index, row) in GEO_ROWS.iter().enumerate() {
        let case = row.case;
        let locations: Vec<String> = match row.locations {
            Locations::None => Vec::new(),
            Locations::TheViewers => vec![viewers_country.clone()],
            Locations::TheViewersAmongOthers => ELSEWHERE
                .iter()
                .map(|code| (*code).to_string())
                .chain([viewers_country.clone()])
                .collect(),
            Locations::Elsewhere => ELSEWHERE.iter().map(|code| (*code).to_string()).collect(),
        };
        let (id, _arn, domain) = topology
            .created_distribution(
                case,
                &config_xml(
                    &format!("edge-geo-published-{index}"),
                    &origin,
                    "",
                    &restrictions(row.restriction_type, &locations),
                ),
            )
            .await;
        topology.awaited_deployment(case, &id).await;
        let answered = Answered::of_stating(case, &domain, "GET", OPEN_PATH, &[]).await;
        if row.refused {
            assert_eq!(
                answered.status, 403,
                "case {case:?} ({locations:?}): expected CloudFront's own 403 at the published \
                 address, got {} with {:?}",
                answered.status, answered.raw
            );
            assert!(
                answered.reports_cache(ERROR),
                "case {case:?}: expected the answer to be reported as CloudFront's own \
                 ({ERROR:?}), got {:?}",
                answered.raw
            );
            assert!(
                !answered.carries_the_origins_bytes(),
                "case {case:?}: a refused viewer must be served none of the origin's bytes, got \
                 {:?}",
                answered.body
            );
            continue;
        }
        assert_eq!(
            answered.status, 200,
            "case {case:?} ({locations:?}): expected the origin's answer through the edge, got \
             {} with {:?}",
            answered.status, answered.raw
        );
        assert!(
            answered.reports_cache(MISS) && answered.carries_the_origins_bytes(),
            "case {case:?}: expected the origin's own bytes for this request ({MISS:?}), got \
             {:?}",
            answered.raw
        );
        // The country the served viewer is reported as being in is the same one the decision
        // was made against: one request has one country, wherever the edge reads it.
        let served_country = reported_country(case, &answered);
        assert_eq!(
            served_country, viewers_country,
            "case {case:?}: expected one country for one viewer, got {served_country:?} after \
             {viewers_country:?}"
        );
    }
}
