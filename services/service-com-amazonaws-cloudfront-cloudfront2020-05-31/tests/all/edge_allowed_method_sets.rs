// SPDX-License-Identifier: CC0-1.0

//! The METHOD GATE of the edge request pipeline, over every set of methods a cache behaviour
//! can carry: which viewer methods the address a deployed distribution published serves at
//! all, and which CloudFront refuses itself without ever asking the origin.
//!
//! The topology is [`crate::edge_lambda_url_origin`]'s — a running Lambda function whose
//! function URL is the origin's `DomainName`, a distribution in front of it, and a viewer that
//! dials the `host:port` the control plane published with no AWS credential — reused rather
//! than copied. What differs here is the distribution: its THREE behaviours carry the three
//! `AllowedMethods` sets the member documents, so one deployed distribution answers the whole
//! rule.
//!
//! The rule the rows span: a viewer request is served from the origin exactly when the
//! behaviour the viewer's path matched lists the request's method in its OWN `AllowedMethods`,
//! and a method that behaviour does not list is refused by CloudFront itself with `403`,
//! `X-Cache: Error from cloudfront`, the sentence CloudFront states for that refusal, and no
//! origin request at all — which is read off Lambda's own `Invocations` series for the
//! function, so "the origin was never invoked" is a fact of the owning service's records
//! rather than an absence in the answer.
//!
//! The rows differ along what the rule generalises over: the method set the matched behaviour
//! carries (each of the three the member publishes), which behaviour the viewer's path matched
//! (the default one and two ordered ones), and the method the viewer sent (every value of the
//! `Method` enum, listed by the matched behaviour or not). The same method is therefore served
//! on one behaviour of a distribution and refused on another, which is what makes the gate the
//! MATCHED behaviour's own decision rather than the distribution's.

use crate::distribution_lifecycle::API_VERSION;
use crate::edge_lambda_url_origin::{Answered, Topology, MISS};
use crate::edge_signed_access::CACHING_DISABLED;

/// `X-Cache` for an answer CloudFront produced itself rather than one an origin did.
const ERROR: &str = "Error from cloudfront";

/// The sentence CloudFront's generated page carries for a viewer method the matched cache
/// behaviour does not allow, as CloudFront words that refusal.
const METHOD_NOT_ALLOWED_DETAIL: &str =
    "This distribution is not configured to allow the HTTP request method that was used for \
     this request. The distribution supports only cachable requests.";

/// Every method a viewer can send, which is every value of the model's `Method` enum
/// (`smithy_query(query="shape", shape="Method")`: `GET`, `HEAD`, `POST`, `PUT`, `PATCH`,
/// `OPTIONS`, `DELETE`).
const EVERY_METHOD: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "OPTIONS", "DELETE"];

/// The three sets `AllowedMethods` can carry, as the member documents them
/// (`smithy_query(query="shape", shape="AllowedMethods")`: "`Quantity` ... Valid values are 2
/// (for `GET` and `HEAD` requests), 3 (for `GET`, `HEAD`, and `OPTIONS` requests) and 7 (for
/// `GET, HEAD, OPTIONS, PUT, PATCH, POST`, and `DELETE` requests)").
const READ_ONLY: &[&str] = &["GET", "HEAD"];
const READ_AND_OPTIONS: &[&str] = &["GET", "HEAD", "OPTIONS"];
const EVERY_ALLOWED_METHOD: &[&str] = &[
    "GET", "HEAD", "OPTIONS", "PUT", "PATCH", "POST", "DELETE",
];

/// One behaviour of the staged distribution: the path a viewer asks for to match it, and the
/// methods its `AllowedMethods` lists.
struct BehaviorRow {
    /// What this behaviour is, named in every assertion of a row served through it.
    case: &'static str,
    /// The path a viewer asks for to match this behaviour.
    viewer_path: &'static str,
    /// The methods this behaviour's `AllowedMethods` lists.
    allowed: &'static [&'static str],
}

/// The behaviours the one deployed distribution carries: the default behaviour and the two
/// ordered ones, each with a different one of the three documented method sets.
const BEHAVIORS: &[BehaviorRow] = &[
    BehaviorRow {
        case: "the default behaviour, which lists GET and HEAD",
        viewer_path: "/whoami",
        allowed: READ_ONLY,
    },
    BehaviorRow {
        case: "an ordered behaviour that lists GET, HEAD and OPTIONS",
        viewer_path: "/preflight/object",
        allowed: READ_AND_OPTIONS,
    },
    BehaviorRow {
        case: "an ordered behaviour that lists every method",
        viewer_path: "/writes/object",
        allowed: EVERY_ALLOWED_METHOD,
    },
];

/// The `AllowedMethods` element listing `methods`, with the `Quantity` the member asks for.
fn allowed_methods(methods: &[&str]) -> String {
    let items: String = methods
        .iter()
        .map(|method| format!("<Method>{method}</Method>"))
        .collect();
    format!(
        "<AllowedMethods><Quantity>{}</Quantity><Items>{items}</Items></AllowedMethods>",
        methods.len()
    )
}

/// One `DistributionConfig` over the function URL `origin` whose default behaviour is
/// [`BEHAVIORS`]'s first row and whose ordered behaviours are the rest, each at the path
/// pattern its row's viewer path falls under.
fn config_xml(caller_reference: &str, origin: &str) -> String {
    let ordered: String = BEHAVIORS[1..]
        .iter()
        .map(|row| {
            let (prefix, _object) = row
                .viewer_path
                .rsplit_once('/')
                .expect("every ordered row's viewer path has a prefix");
            format!(
                "<CacheBehavior><PathPattern>{prefix}/*</PathPattern>\
                 <TargetOriginId>function-url</TargetOriginId>\
                 <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{}\
                 <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
                 </CacheBehavior>",
                allowed_methods(row.allowed)
            )
        })
        .collect();
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
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{}\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>{}</Quantity><Items>{ordered}</Items></CacheBehaviors>\
         <Comment>one function url origin behind every allowed-method set</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        allowed_methods(BEHAVIORS[0].allowed),
        BEHAVIORS.len() - 1,
    )
}

/// RULE: the edge serves a viewer method only when the behaviour the viewer's path matched
/// lists it in that behaviour's own `AllowedMethods`; every other method is refused by
/// CloudFront itself with `403` and makes no origin request.
///
/// The rows differ along the method set the matched behaviour carries (the three the member
/// publishes), the behaviour the path matched (the default and two ordered ones) and the method
/// the viewer sent (every value of the `Method` enum).
#[tokio::test]
async fn each_behaviours_allowed_method_set_decides_which_viewer_methods_are_served() {
    let topology = Topology::new();
    let case = "every allowed-method set behind one published edge domain";
    let function = "cf-edge-method-sets";
    let origin = topology.public_function_url_host(case, function).await;
    let (id, _arn, domain) = topology
        .created_distribution(case, &config_xml("edge-allowed-method-sets", &origin))
        .await;
    topology.awaited_deployment(case, &id).await;

    for behavior in BEHAVIORS {
        for method in EVERY_METHOD {
            let allowed = behavior.allowed.contains(method);
            let row = format!(
                "{method} on {} ({})",
                behavior.viewer_path, behavior.case
            );
            let before = topology.invocations(&row, function).await;
            let answered =
                Answered::of_stating(&row, &domain, method, behavior.viewer_path, &[]).await;
            let after = topology.invocations(&row, function).await;
            if allowed {
                assert_eq!(
                    answered.status, 200,
                    "case {row:?}: a method the matched behaviour lists must be served from \
                     the origin, got {} with {:?}",
                    answered.status, answered.raw
                );
                assert!(
                    answered.reports_cache(MISS),
                    "case {row:?}: expected an answer the origin produced for this request \
                     ({MISS:?}), got {:?}",
                    answered.raw
                );
                assert!(
                    after > before,
                    "case {row:?}: a method the matched behaviour lists must reach the origin, \
                     but the function's invocations stayed at {before} (now {after})"
                );
                // A `HEAD` carries no body, so only the other methods can report what the
                // origin saw of the viewer's request.
                if *method != "HEAD" {
                    assert!(
                        answered.carries_the_origins_bytes(),
                        "case {row:?}: expected the origin's own bytes, got {:?}",
                        answered.body
                    );
                    let report = answered.report(&row);
                    assert_eq!(
                        report["method"].as_str().unwrap_or_default(),
                        *method,
                        "case {row:?}: expected the origin to see the viewer's method, got \
                         {report}"
                    );
                    assert_eq!(
                        report["path"].as_str().unwrap_or_default(),
                        behavior.viewer_path,
                        "case {row:?}: expected the origin to see the viewer's path, got \
                         {report}"
                    );
                }
                continue;
            }
            assert_eq!(
                answered.status, 403,
                "case {row:?}: a method the matched behaviour does not list must be refused by \
                 CloudFront itself, got {} with {:?}",
                answered.status, answered.raw
            );
            assert!(
                answered.body.contains(METHOD_NOT_ALLOWED_DETAIL),
                "case {row:?}: expected the sentence CloudFront states for a method its \
                 configuration does not allow, got {:?}",
                answered.body
            );
            assert!(
                !answered.carries_the_origins_bytes(),
                "case {row:?}: expected none of the origin's bytes in a refusal CloudFront \
                 produced, got {:?}",
                answered.body
            );
            assert!(
                answered.reports_cache(ERROR),
                "case {row:?}: expected the refusal reported as CloudFront's own answer \
                 ({ERROR:?}), got {:?}",
                answered.raw
            );
            assert_eq!(
                after, before,
                "case {row:?}: a method the matched behaviour does not list must make no origin \
                 request, but the function's invocations went from {before} to {after}"
            );
        }
    }
}
