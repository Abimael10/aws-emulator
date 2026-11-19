// SPDX-License-Identifier: CC0-1.0

//! Whether a client can OBSERVE WAFv2 rule evaluation on a load balancer it protected: the
//! same request sent again on the address the load balancer published, and the binding the
//! control plane reports for that load balancer.
//!
//! RULE: **an address an operation publishes for a client to connect to is an address this
//! emulator serves, and the web ACL `AssociateWebACL` bound to a protected resource's ARN
//! decides every request that arrives on it from the REQUEST alone — so a request repeated on
//! that address is decided the same way every time, in any order with other requests, and the
//! web ACL that decides it is the one `GetWebACLForResource` reports for the resource and
//! `ListResourcesForWebACL` lists the resource under.**
//!
//! `LoadBalancer$DNSName` is "the public DNS name of the load balancer", the address a client
//! is handed and then dials, and `AssociateWebACLRequest$ResourceArn` names an Application
//! Load Balancer as a protected resource. `GetWebACLForResourceResponse$WebACL` is documented
//! to come back null where a resource carries no binding ("if there is no associated resource,
//! WAF returns a null web ACL"), and `ListResourcesForWebACLRequest$ResourceType` defaults to
//! `APPLICATION_LOAD_BALANCER`, so those two readers are what a client asks about a load
//! balancer's protection.
//!
//! Why this suite stands beside [`super::elbv2_published_address_enforces_its_web_acl`], which
//! reads the same edge: that sibling's table sends ONE request per web ACL and so says nothing
//! about a decision asked for twice. A client verifying its rules sends an allowed request, a
//! blocked one, and then the allowed one again, and that sequence only holds if deciding a
//! request consumes nothing — no counter, no quota and no binding. The rows here send each
//! request shape in several rounds and compare the rounds, and they read the binding back
//! through the control plane, which no row of the sibling does.
//!
//! The rows differ along what the rule generalises over: the component of the request the
//! blocking rule inspects (a header, the URI path), a matching and a non-matching request for
//! each, the method, the round the request is sent in, which of two web ACLs is bound, and no
//! binding at all.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_published_address_enforces_its_web_acl::{
    build_router, byte_match, create_web_acl, dialable_port, exchange, load_balancer, network,
    rule, waf_ok, WebAclSpec, BLOCK_HEADER, BLOCK_STATUS, LISTENER_BODY,
};

/// The path the one path-blocking web ACL of this suite refuses.
const BLOCKED_PATH: &str = "/admin";

/// How many times the whole sequence of requests is sent. More than two, so a decision that
/// drifts after the first repeat is caught as well as one that drifts on it.
const ROUNDS: usize = 3;

/// One request of the sequence, and the status it must be answered with while the web ACL
/// that blocks on [`BLOCK_HEADER`] is in front of the load balancer.
struct Sent {
    case: &'static str,
    method: &'static str,
    target: &'static str,
    headers: &'static [(&'static str, &'static str)],
    status: u16,
}

/// The sequence one round sends: an allowed request, a blocked one, the allowed one again, and
/// the request shapes around them that the same web ACL must decide the other way.
fn sequence() -> Vec<Sent> {
    vec![
        Sent {
            case: "a plain GET the web ACL lets through",
            method: "GET",
            target: "/",
            headers: &[],
            status: 200,
        },
        Sent {
            case: "the same GET carrying the blocked header",
            method: "GET",
            target: "/",
            headers: &[(BLOCK_HEADER, "1")],
            status: BLOCK_STATUS,
        },
        Sent {
            case: "the plain GET again, after the block",
            method: "GET",
            target: "/",
            headers: &[],
            status: 200,
        },
        Sent {
            case: "the blocked header again, after the allow",
            method: "GET",
            target: "/",
            headers: &[(BLOCK_HEADER, "1")],
            status: BLOCK_STATUS,
        },
        Sent {
            case: "another method carrying the blocked header",
            method: "POST",
            target: "/",
            headers: &[(BLOCK_HEADER, "1")],
            status: BLOCK_STATUS,
        },
        Sent {
            case: "another path carrying the blocked header",
            method: "GET",
            target: "/orders?page=2",
            headers: &[(BLOCK_HEADER, "1")],
            status: BLOCK_STATUS,
        },
        Sent {
            case: "the header carrying a value the rule does not match",
            method: "GET",
            target: "/",
            headers: &[(BLOCK_HEADER, "0")],
            status: 200,
        },
        Sent {
            case: "another path with no blocked header",
            method: "GET",
            target: "/orders?page=2",
            headers: &[],
            status: 200,
        },
    ]
}

/// A web ACL whose default action allows and whose one rule blocks a request whose
/// [`BLOCK_HEADER`] is exactly `1`.
fn blocks_the_header() -> WebAclSpec {
    WebAclSpec::allowing(json!([rule(
        "block-header",
        byte_match(
            json!({ "SingleHeader": { "Name": BLOCK_HEADER } }),
            "1",
            "EXACTLY",
        ),
        json!({ "Block": {} }),
    )]))
}

/// A web ACL whose default action allows and whose one rule blocks a request for
/// [`BLOCKED_PATH`]: the other web ACL of this suite, so which binding is in place decides
/// which of two requests is refused.
fn blocks_the_path() -> WebAclSpec {
    WebAclSpec::allowing(json!([rule(
        "block-path",
        byte_match(json!({ "UriPath": {} }), BLOCKED_PATH, "EXACTLY"),
        json!({ "Block": {} }),
    )]))
}

/// The ARN of the web ACL `GetWebACLForResource` reports in front of `resource`, or `None`
/// where it reports none.
async fn web_acl_in_front_of(router: &MultiServiceRouter, resource: &str) -> Option<String> {
    let reported = waf_ok(
        router,
        "GetWebACLForResource",
        json!({ "ResourceArn": resource }),
    )
    .await;
    reported["WebACL"]["ARN"].as_str().map(str::to_string)
}

/// The resources `ListResourcesForWebACL` lists under `web_acl`.
async fn resources_of(router: &MultiServiceRouter, web_acl: &str) -> Vec<String> {
    let listed = waf_ok(
        router,
        "ListResourcesForWebACL",
        json!({ "WebACLArn": web_acl }),
    )
    .await;
    listed["ResourceArns"]
        .as_array()
        .map(|arns| {
            arns.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: a request repeated on the published address is decided the same way every time, and
/// an allowed request is still allowed after a blocked one — deciding a request consumes
/// nothing.
#[tokio::test]
async fn the_edge_decides_a_repeated_request_the_same_way_every_round() {
    let router = build_router();
    let (subnets, security_group) = network(&router).await;
    let (lb_arn, published) =
        load_balancer(&router, "waf-repeat-alb", &subnets, &security_group).await;
    // The published address is one a client of this emulator can dial at all, which is what
    // every request below is sent to.
    dialable_port("the address the load balancer published", &published);

    let web_acl = create_web_acl(&router, "repeat-acl", &blocks_the_header()).await;
    waf_ok(
        &router,
        "AssociateWebACL",
        json!({ "ResourceArn": lb_arn, "WebACLArn": web_acl }),
    )
    .await;

    for round in 1..=ROUNDS {
        for sent in sequence() {
            let case = format!("round {round}: {}", sent.case);
            let answer = exchange(&case, &published, sent.method, sent.target, sent.headers).await;
            assert_eq!(
                answer.status, sent.status,
                "case {case:?}: expected HTTP {}, got {} in {:?}",
                sent.status, answer.status, answer.raw
            );
            // A request the web ACL let through is answered by the LISTENER; a refused one is
            // answered at the edge, where none of the listener's actions runs.
            let served_by_the_listener = answer.raw.contains(LISTENER_BODY);
            let expected_from_the_listener = sent.status == 200;
            assert_eq!(
                served_by_the_listener, expected_from_the_listener,
                "case {case:?}: expected the listener's own answer carried \
                 {expected_from_the_listener:?}, got {served_by_the_listener:?} in {:?}",
                answer.raw
            );
        }
    }
}

/// What a stage of the binding table does to the web ACL in front of the load balancer before
/// it reads the control plane and sends its two requests.
enum Bind {
    /// Leave the load balancer as it is, which at the first stage is unprotected.
    Nothing,
    /// `AssociateWebACL` the web ACL blocking on the header, or the one blocking on the path.
    OnHeader,
    OnPath,
    /// `DisassociateWebACL`: nothing is in front of the load balancer afterwards.
    Away,
}

/// One stage: what it binds, and the status each of the two requests must then be answered
/// with.
struct Stage {
    case: &'static str,
    bind: Bind,
    /// The status of a request carrying the header the header-blocking web ACL refuses.
    header_status: u16,
    /// The status of a request for the path the path-blocking web ACL refuses.
    path_status: u16,
}

/// Every stage of the binding table: unprotected, each web ACL in turn, and unprotected again.
fn stages() -> Vec<Stage> {
    vec![
        Stage {
            case: "no web ACL bound to the load balancer",
            bind: Bind::Nothing,
            header_status: 200,
            path_status: 200,
        },
        Stage {
            case: "the header-blocking web ACL bound",
            bind: Bind::OnHeader,
            header_status: BLOCK_STATUS,
            path_status: 200,
        },
        Stage {
            case: "the path-blocking web ACL bound in its place",
            bind: Bind::OnPath,
            header_status: 200,
            path_status: BLOCK_STATUS,
        },
        Stage {
            case: "the binding taken away again",
            bind: Bind::Away,
            header_status: 200,
            path_status: 200,
        },
    ]
}

/// RULE: the web ACL the edge applies is the binding the control plane holds for the protected
/// resource — the one `GetWebACLForResource` reports and `ListResourcesForWebACL` lists the
/// resource under — so replacing it changes which request is refused, and removing it refuses
/// none.
#[tokio::test]
async fn the_edge_applies_the_binding_the_control_plane_reports() {
    let router = build_router();
    let (subnets, security_group) = network(&router).await;
    let (lb_arn, published) =
        load_balancer(&router, "waf-binding-alb", &subnets, &security_group).await;

    let on_header = create_web_acl(&router, "binding-acl-header", &blocks_the_header()).await;
    let on_path = create_web_acl(&router, "binding-acl-path", &blocks_the_path()).await;

    for stage in stages() {
        let expected_in_front = match stage.bind {
            Bind::Nothing => None,
            Bind::OnHeader | Bind::OnPath => {
                let web_acl = if matches!(stage.bind, Bind::OnHeader) {
                    &on_header
                } else {
                    &on_path
                };
                waf_ok(
                    &router,
                    "AssociateWebACL",
                    json!({ "ResourceArn": lb_arn, "WebACLArn": web_acl }),
                )
                .await;
                Some(web_acl.clone())
            }
            Bind::Away => {
                waf_ok(
                    &router,
                    "DisassociateWebACL",
                    json!({ "ResourceArn": lb_arn }),
                )
                .await;
                None
            }
        };

        let in_front = web_acl_in_front_of(&router, &lb_arn).await;
        assert_eq!(
            in_front, expected_in_front,
            "case {:?}: expected GetWebACLForResource to report {expected_in_front:?} in front \
             of the load balancer, got {in_front:?}",
            stage.case
        );
        for (which, web_acl) in [("header-blocking", &on_header), ("path-blocking", &on_path)] {
            let listed = resources_of(&router, web_acl).await;
            let lists_it = listed.contains(&lb_arn);
            let should_list_it = expected_in_front.as_deref() == Some(web_acl.as_str());
            assert_eq!(
                lists_it, should_list_it,
                "case {:?}: expected ListResourcesForWebACL of the {which} web ACL to list the \
                 load balancer {should_list_it:?}, got {listed:?}",
                stage.case
            );
        }

        let header_case = format!("{}: the request carrying the blocked header", stage.case);
        let on_the_header = exchange(
            &header_case,
            &published,
            "GET",
            "/",
            &[(BLOCK_HEADER, "1")],
        )
        .await;
        assert_eq!(
            on_the_header.status, stage.header_status,
            "case {header_case:?}: expected HTTP {}, got {} in {:?}",
            stage.header_status, on_the_header.status, on_the_header.raw
        );

        let path_case = format!("{}: the request for the blocked path", stage.case);
        let on_the_path = exchange(&path_case, &published, "GET", BLOCKED_PATH, &[]).await;
        assert_eq!(
            on_the_path.status, stage.path_status,
            "case {path_case:?}: expected HTTP {}, got {} in {:?}",
            stage.path_status, on_the_path.status, on_the_path.raw
        );
    }
}
