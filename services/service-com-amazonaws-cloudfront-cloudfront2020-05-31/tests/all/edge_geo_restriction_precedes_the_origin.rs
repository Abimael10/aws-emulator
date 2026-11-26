// SPDX-License-Identifier: CC0-1.0

//! The distribution's GEOGRAPHIC RESTRICTION decided BEFORE the origin is contacted, over a
//! LAMBDA FUNCTION URL origin, end to end at the address the distribution published.
//!
//! RULE: a distribution's `Restrictions.GeoRestriction` is an ADMISSION decision the edge makes
//! for a viewer request before any origin request exists — `blacklist` naming the viewer's
//! country, and `whitelist` leaving it out, are refused by CloudFront itself with `403`,
//! `X-Cache: Error from cloudfront` and none of the origin's bytes, and the ORIGIN IS NEVER
//! INVOKED; every other viewer is served the origin's own answer and is told the very country
//! the restriction was evaluated against (`CloudFront-Viewer-Country`, "a two-letter, uppercase
//! country code", where the matched behaviour's origin request policy names it).
//!
//! What this suite adds to [`crate::edge_geo_restriction`] (the same rule over an origin this
//! emulator's control plane serves) and to [`crate::edge_lambda_url_origin`]'s geo rows (the
//! same rule over a function URL named by its authority) is the half of the rule an ANSWER
//! cannot carry: "the origin was never contacted" is read off LAMBDA's own `Invocations` series
//! for the function behind the origin, so a refusal that nonetheless relayed the request to the
//! origin — and an admitted viewer the edge answered without reaching it — both fail here. An
//! edge that forwarded a blocked country to the origin and then dropped the answer is
//! indistinguishable from a correct refusal by the status alone; it is not indistinguishable
//! from one by the owning service's records.
//!
//! The rows differ along what the rule generalises over: the restriction type (`blacklist`,
//! `whitelist`), how many locations the restriction names, whether the viewer's country is one
//! of them, the case the viewer states it in — a `Location` is a code rather than a spelling —
//! and a viewer that states no country at all.
//!
//! How a viewer states where it is: CloudFront resolves a viewer's country from its ADDRESS,
//! through the MaxMind GeoIP databases `GeoRestriction` documents. Every viewer of this emulator
//! dials a loopback address, which is in no country those databases place, so the country is
//! stated on the request itself, in the header CloudFront carries it in — and a viewer that
//! states none is in the country the edge reports for it.
//!
//! The topology, the function behind the origin and the reader of one answer off the connection
//! are [`crate::edge_lambda_url_origin`]'s, imported rather than repeated.

use serde_json::Value;

use crate::distribution_lifecycle::API_VERSION;
use crate::edge_lambda_url_origin::{
    Answered, Topology, ALL_VIEWER_AND_CLOUDFRONT, MISS, OPEN_PATH,
};
use crate::edge_signed_access::CACHING_DISABLED;

/// The header a viewer states its country in, as CloudFront spells it.
const VIEWER_COUNTRY_HEADER: &str = "cloudfront-viewer-country";

/// `X-Cache` for an answer CloudFront produced itself.
const ERROR: &str = "Error from cloudfront";

/// One viewer of a distribution the table drives: the country it states (`None` for a viewer
/// that states none) and whether the distribution's restriction refuses it.
struct ViewerRow {
    case: &'static str,
    stated_country: Option<&'static str>,
    refused: bool,
}

/// A `Restrictions` element of `restriction_type` over `locations`; `Quantity` is the number of
/// locations it carries, which is what the control plane checks it against.
fn restrictions(restriction_type: &str, locations: &[&str]) -> String {
    let items: String = locations
        .iter()
        .map(|location| format!("<Location>{location}</Location>"))
        .collect();
    let items = if locations.is_empty() {
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

/// One `DistributionConfig` over the function URL `origin` carrying `restrictions`: the
/// AWS-managed `CachingDisabled` cache policy, so every row's request is an origin fetch rather
/// than another row's cached answer — which is what makes the invocation count below readable —
/// and the managed origin request policy that forwards the `CloudFront-*` headers the edge
/// generates, without which the origin is told no country at all.
fn config_xml(caller_reference: &str, origin: &str, restrictions: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>function-url</Id><DomainName>{origin}</DomainName>\
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
         <Comment>a geo-restricted function url origin</Comment>\
         {restrictions}\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// Drive every row of one table against ONE deployed distribution over a running function URL,
/// holding each answer AND the origin's own invocation count to the rule.
///
/// `restriction` is the whole `Restrictions` element the distribution carries, and the function
/// behind the origin is staged here, so a table states nothing about the staging.
async fn each_viewer_of(
    suite: &str,
    function: &str,
    restriction: &str,
    rows: &[ViewerRow],
) {
    let topology = Topology::new();
    let origin = topology.public_function_url_host(suite, function).await;
    let (id, _arn, edge) = topology
        .created_distribution(suite, &config_xml(function, &origin, restriction))
        .await;
    topology.awaited_deployment(suite, &id).await;

    for row in rows {
        let case = row.case;
        let stated: Vec<(&str, &str)> = row
            .stated_country
            .map(|country| vec![(VIEWER_COUNTRY_HEADER, country)])
            .unwrap_or_default();
        // The origin's own count of the invocations it has served, read on each side of this
        // viewer request: the question "did this request reach the origin" is answered by the
        // service that owns the function rather than by what came back.
        let before = topology.invocations(case, function).await;
        let answered = Answered::of_stating(case, &edge, "GET", OPEN_PATH, &stated).await;
        let after = topology.invocations(case, function).await;

        if row.refused {
            assert_eq!(
                answered.status, 403,
                "case {case:?}: expected CloudFront's own 403 for a viewer the restriction \
                 refuses, got {} with {:?}",
                answered.status, answered.raw
            );
            assert!(
                answered.reports_cache(ERROR),
                "case {case:?}: expected the refusal reported as CloudFront's own answer \
                 ({ERROR:?}), got {:?}",
                answered.raw
            );
            assert!(
                !answered.carries_the_origins_bytes(),
                "case {case:?}: expected none of the origin's bytes in a refusal CloudFront \
                 produced, got {:?}",
                answered.body
            );
            assert_eq!(
                after, before,
                "case {case:?}: a viewer the geographic restriction refuses must make no origin \
                 request, but the function's invocations went from {before} to {after}"
            );
            continue;
        }

        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the origin's own answer through the edge, got {} with {:?}",
            answered.status, answered.raw
        );
        assert!(
            answered.reports_cache(MISS),
            "case {case:?}: expected an answer the origin produced for this request ({MISS:?}), \
             got {:?}",
            answered.raw
        );
        assert!(
            answered.carries_the_origins_bytes(),
            "case {case:?}: expected the function's own bytes through the edge, got {:?}",
            answered.body
        );
        assert!(
            after > before,
            "case {case:?}: a viewer the restriction admits must be served from the origin, but \
             the function's invocations stayed at {before} (now {after})"
        );
        // The country the edge TOLD the origin is the one value this request's country has, so
        // it is the country the restriction was evaluated against — and it is a code whichever
        // spelling the viewer stated, or none at all.
        let report: Value = answered.report(case);
        let told = report["country"].as_str().unwrap_or_default().to_string();
        assert!(
            told.len() == 2 && told.chars().all(|letter| letter.is_ascii_uppercase()),
            "case {case:?}: expected the origin to be told a two-letter uppercase country code, \
             got {told:?} in {report}"
        );
        if let Some(stated) = row.stated_country {
            assert_eq!(
                told,
                stated.to_ascii_uppercase(),
                "case {case:?}: expected the origin to be told the country the edge admitted \
                 this viewer in, got {told:?} in {report}"
            );
        }
    }
}

/// RULE, over a `blacklist`: the locations it names are the countries CloudFront does not
/// distribute the content in, so a viewer in one of them is refused at the edge with no origin
/// request made at all, while a viewer outside them is served from the origin.
#[tokio::test]
async fn a_blocklisted_viewer_never_reaches_a_function_url_origin() {
    // The rows differ along whether the viewer's country is one the blocklist names, the case
    // the viewer spells it in, and which of several named countries it is.
    let rows: &[ViewerRow] = &[
        ViewerRow {
            case: "a country the blocklist names",
            stated_country: Some("RU"),
            refused: true,
        },
        ViewerRow {
            case: "the same country stated in lower case",
            stated_country: Some("ru"),
            refused: true,
        },
        ViewerRow {
            case: "the last of the countries the blocklist names",
            stated_country: Some("KP"),
            refused: true,
        },
        ViewerRow {
            case: "a country outside the blocklist",
            stated_country: Some("DE"),
            refused: false,
        },
        ViewerRow {
            case: "a viewer that states no country, under a blocklist naming none of its own",
            stated_country: None,
            refused: false,
        },
    ];
    each_viewer_of(
        "a blocklisted function URL origin",
        "cf-geo-blocklist-origin",
        &restrictions("blacklist", &["RU", "CN", "KP"]),
        rows,
    )
    .await;
}

/// RULE, over a `whitelist`: the locations it names are the countries CloudFront DOES
/// distribute the content in, so a viewer anywhere else — including one that states no country
/// — is refused at the edge with no origin request made, while a viewer in a named country is
/// served from the origin.
#[tokio::test]
async fn an_allowlist_decides_which_viewers_reach_a_function_url_origin() {
    // The rows differ along whether the viewer's country is one the allowlist names, the case
    // it is stated in, and a viewer that states none at all.
    let rows: &[ViewerRow] = &[
        ViewerRow {
            case: "a country the allowlist names",
            stated_country: Some("DE"),
            refused: false,
        },
        ViewerRow {
            case: "another country it names, stated in lower case",
            stated_country: Some("fr"),
            refused: false,
        },
        ViewerRow {
            case: "a country the allowlist leaves out",
            stated_country: Some("RU"),
            refused: true,
        },
        ViewerRow {
            case: "a viewer that states no country, under an allowlist naming other countries",
            stated_country: None,
            refused: true,
        },
    ];
    each_viewer_of(
        "an allowlisted function URL origin",
        "cf-geo-allowlist-origin",
        &restrictions("whitelist", &["DE", "FR"]),
        rows,
    )
    .await;
}
