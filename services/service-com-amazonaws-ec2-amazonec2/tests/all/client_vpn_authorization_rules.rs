// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the INGRESS AUTHORIZATION RULES of a Client VPN endpoint:
//! `AuthorizeClientVpnIngress`, the `DescribeClientVpnAuthorizationRules` that reads its
//! writes back, the `RevokeClientVpnIngress` that takes one off again, and the
//! `DeleteClientVpnEndpoint` that takes the endpoint itself down.
//!
//! Driven over the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=<Operation>`, answered
//! with XML (README.md / OPERATIONS.md "Wire protocol"). ec2Query is selected by the `ec2`
//! signing name in the request's SigV4 credential scope, so every call here carries one.
//!
//! The rules under test:
//!
//! 1. `AuthorizeClientVpnIngress` ADDS ONE RULE TO THE ENDPOINT THE REQUEST NAMES, from the
//!    network it carries and whoever it grants that network to — one Active Directory or
//!    identity-provider group, or every client — answers it `authorizing` (the clock owes it
//!    `active` a transition span later), and holds it under the endpoint + network + group
//!    that name it: so a second request for that same triple is a duplicate rather than a
//!    second rule, while another network, another group, or another endpoint is a rule of its
//!    own.
//! 2. `DescribeClientVpnAuthorizationRules` reports the rules of the endpoint the request
//!    names, with the members the model declares, narrowed by each filter its own
//!    documentation publishes.
//! 3. `RevokeClientVpnIngress` takes off exactly the rule it names — the all-groups rule when
//!    it asks for all groups, and one group's rule when it names that group — answers it
//!    `revoking`, and leaves every other rule of the endpoint in place; a rule the endpoint
//!    does not have is refused rather than reported as revoked.
//! 4. `DeleteClientVpnEndpoint` answers the endpoint `deleting` and the clock owes it
//!    `deleted`, and an endpoint that still has a target network associated is refused — "you
//!    must disassociate all target networks before you can delete a Client VPN endpoint".
//!
//! Every table row below is a distinct input along what those rules generalise over: the size
//! of the network block, whether the grant is to one group or to every client, whether a
//! description is carried, which endpoint the rule is on, and — for the refusals — which part
//! of the request cannot be served.

use std::time::{Duration, Instant};

use ec2::ops::types::ClientVpnEndpoint;
use ec2::state::{
    ClientVpnTargetNetworkSpec as TargetNetworkSpec, ComAmazonawsEc2Amazonec2State as Ec2State,
    CLIENT_VPN_AUTHORIZATION_RULE_STATUS_ACTIVE, CLIENT_VPN_AUTHORIZATION_RULE_STATUS_AUTHORIZING,
    CLIENT_VPN_AUTHORIZATION_RULE_STATUS_REVOKING, CLIENT_VPN_ENDPOINT_STATUS_DELETED,
    CLIENT_VPN_ENDPOINT_STATUS_DELETING, INVALID_CLIENT_VPN_DUPLICATE_AUTHORIZATION_RULE,
    INVALID_CLIENT_VPN_ENDPOINT_AUTHORIZATION_RULE_NOT_FOUND,
    INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, filters};

/// The Region every call here is signed for. The Client VPN stores are per account rather
/// than per Region, so the Region only selects the ec2Query envelope.
const REGION: &str = "us-east-1";

/// The actions this suite drives.
const AUTHORIZE: &str = "AuthorizeClientVpnIngress";
const DESCRIBE: &str = "DescribeClientVpnAuthorizationRules";
const REVOKE: &str = "RevokeClientVpnIngress";
const DELETE_ENDPOINT: &str = "DeleteClientVpnEndpoint";

/// A well-formed endpoint id this account holds no endpoint for: what the refusal rows name.
const ABSENT_ENDPOINT: &str = "cvpn-endpoint-0123456789abcdef1";

/// The VPC the seeded endpoint is attached to.
const ENDPOINT_VPC: &str = "vpc-00000000000000001";

/// The deadline every poll below gives up at, and how often it looks again. Generous: what is
/// waited for is the CONDITION a clock-owed move settles into, so nothing here depends on how
/// fast the host runs — all the deadline bounds is a hang, and the interval is short enough
/// that it costs many looks rather than one.
const POLL_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// One form field.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One call of `action` carrying `params`, as `(status, body)`.
async fn call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// Write one Client VPN endpoint, the way `CreateClientVpnEndpoint` would, and answer the
/// `ClientVpnEndpointId` it was minted with.
fn seed_endpoint(engine: &engine::InMemoryEngine, description: &str) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_client_vpn_endpoint(ClientVpnEndpoint {
            description: Some(description.to_string()),
            client_cidr_block: Some("10.100.0.0/22".to_string()),
            vpc_id: Some(ENDPOINT_VPC.to_string()),
            ..Default::default()
        })
        .expect("the store must take the Client VPN endpoint")
}

/// Associate one target network with `endpoint_id`, the way `AssociateClientVpnTargetNetwork`
/// would — what makes the endpoint one that cannot be deleted yet.
fn seed_target_network(engine: &engine::InMemoryEngine, endpoint_id: &str) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_client_vpn_target_network(TargetNetworkSpec {
            client_vpn_endpoint_id: endpoint_id.to_string(),
            target_network_id: Some("subnet-00000000000000001".to_string()),
            vpc_id: Some(ENDPOINT_VPC.to_string()),
            ..Default::default()
        })
        .and_then(|network| network.association_id)
        .expect("the store must take the target network association")
}

/// The form fields of one authorize: the endpoint, the network, who it is granted to, and
/// optionally a description.
fn authorize_params(
    endpoint_id: &str,
    network: &str,
    group: Option<&str>,
    description: Option<&str>,
) -> Vec<(String, String)> {
    let mut params = vec![
        p("ClientVpnEndpointId", endpoint_id),
        p("TargetNetworkCidr", network),
    ];
    match group {
        Some(group) => params.push(p("AccessGroupId", group)),
        None => params.push(p("AuthorizeAllGroups", "true")),
    }
    if let Some(description) = description {
        params.push(p("Description", description));
    }
    params
}

/// The form fields of one revoke, named the same way.
fn revoke_params(endpoint_id: &str, network: &str, group: Option<&str>) -> Vec<(String, String)> {
    let mut params = vec![
        p("ClientVpnEndpointId", endpoint_id),
        p("TargetNetworkCidr", network),
    ];
    match group {
        Some(group) => params.push(p("AccessGroupId", group)),
        None => params.push(p("RevokeAllGroups", "true")),
    }
    params
}

/// The one `ClientVpnEndpointId` parameter every describe carries.
fn endpoint_param(endpoint_id: &str) -> Vec<(String, String)> {
    vec![p("ClientVpnEndpointId", endpoint_id)]
}

/// The networks the describe reports rules for on `endpoint_id`, sorted so the set is
/// compared rather than the order.
async fn reported_networks(
    router: &MultiServiceRouter,
    case: &str,
    endpoint_id: &str,
) -> Vec<String> {
    let (status, body) = call(router, DESCRIBE, &endpoint_param(endpoint_id)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {DESCRIBE}, got {status} with body {body}"
    );
    let mut networks = elements(&body, "destinationCidr");
    networks.sort();
    networks
}

/// One row of the authorize table: the network a rule covers, who it is granted to, the
/// description the request carries, and the members the describe must report back.
struct Row {
    case: &'static str,
    network: &'static str,
    group: Option<&'static str>,
    description: Option<&'static str>,
}

/// The authorizes the report below is written against. They differ along every axis the rule
/// generalises over: the size of the network block (a whole /8, a /16, a /24 and a single
/// /32 host), whether the grant is to one group or to every client, and whether a description
/// is carried.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a /16 granted to every client",
            network: "10.0.0.0/16",
            group: None,
            description: Some("the application network"),
        },
        Row {
            case: "a /8 granted to one directory group, no description",
            network: "172.0.0.0/8",
            group: Some("S-1-5-21-1004336348-1177238915-682003330-512"),
            description: None,
        },
        Row {
            case: "a /24 granted to one identity-provider group",
            network: "192.168.4.0/24",
            group: Some("engineering"),
            description: Some("the engineering subnet"),
        },
        Row {
            case: "a single host granted to every client",
            network: "203.0.113.7/32",
            group: None,
            description: Some("one host"),
        },
    ]
}

/// Rule 1 and rule 2: what an authorize stores is what the describe reports back — the
/// network, who it grants that network to, the description the request carried and the
/// `authorizing` state the rule starts in.
#[tokio::test]
async fn an_authorized_rule_is_reported_with_the_members_the_request_carried() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = super::ec2_query::emulator();
        let endpoint = seed_endpoint(&engine, case);

        let (status, body) = call(
            &router,
            AUTHORIZE,
            &authorize_params(&endpoint, row.network, row.group, row.description),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {AUTHORIZE}, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "code").as_deref(),
            Some(CLIENT_VPN_AUTHORIZATION_RULE_STATUS_AUTHORIZING),
            "case {case:?}: the authorize answers the rule authorizing, got {body}"
        );

        let (status, reported) = call(&router, DESCRIBE, &endpoint_param(&endpoint)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {DESCRIBE}, got {status} with body {reported}"
        );
        assert_eq!(
            element(&reported, "clientVpnEndpointId").as_deref(),
            Some(endpoint.as_str()),
            "case {case:?}: the rule is reported on the endpoint it was added to, got {reported}"
        );
        assert_eq!(
            element(&reported, "destinationCidr").as_deref(),
            Some(row.network),
            "case {case:?}: expected the network the request carried, got {reported}"
        );
        assert_eq!(
            element(&reported, "description").as_deref(),
            row.description,
            "case {case:?}: expected the description the request carried, got {reported}"
        );
        assert_eq!(
            element(&reported, "groupId").as_deref(),
            row.group,
            "case {case:?}: expected the group the request granted the network to, got {reported}"
        );
        assert_eq!(
            element(&reported, "accessAll").as_deref(),
            Some(if row.group.is_none() { "true" } else { "false" }),
            "case {case:?}: a rule granted to every client reports accessAll, got {reported}"
        );

        // The move the clock owes the rule, looked for until it holds rather than waited out.
        let settled = poll_until_rule_status(
            case,
            &router,
            &endpoint,
            CLIENT_VPN_AUTHORIZATION_RULE_STATUS_ACTIVE,
        )
        .await;
        assert_eq!(
            settled.as_deref(),
            Some(CLIENT_VPN_AUTHORIZATION_RULE_STATUS_ACTIVE),
            "case {case:?}: the clock owes the rule active, got {settled:?}"
        );
    }
}

/// Look at the endpoint's rules AGAIN until the describe reports `wanted`, or a deadline far
/// past the service's own transition span passes. Answers the status last seen.
async fn poll_until_rule_status(
    case: &str,
    router: &MultiServiceRouter,
    endpoint_id: &str,
    wanted: &str,
) -> Option<String> {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = call(router, DESCRIBE, &endpoint_param(endpoint_id)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {DESCRIBE}, got {status} with body {body}"
        );
        let held = element(&body, "code");
        if held.as_deref() == Some(wanted) || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Rule 1: one endpoint grants one network to one group ONCE — a second authorize of the same
/// triple is a duplicate — while another network, another group and another endpoint are each
/// a rule of their own.
#[tokio::test]
async fn one_network_is_granted_to_one_group_once_per_endpoint() {
    let (router, engine) = super::ec2_query::emulator();
    let endpoint = seed_endpoint(&engine, "the endpoint the rules are on");
    let other_endpoint = seed_endpoint(&engine, "another endpoint");
    let network = "10.1.0.0/16";
    let group = "engineering";

    for params in [
        authorize_params(&endpoint, network, Some(group), None),
        // The SAME network granted to another group is a second rule, and so is another
        // network granted to the same group.
        authorize_params(&endpoint, network, Some("finance"), None),
        authorize_params(&endpoint, "10.2.0.0/16", Some(group), None),
        // The all-groups rule for a network one group already has is a rule of its own too.
        authorize_params(&endpoint, network, None, None),
    ] {
        let (status, body) = call(&router, AUTHORIZE, &params).await;
        assert_eq!(
            status, 200,
            "the first authorize of each triple succeeds: got {status} with body {body} for \
             {params:?}"
        );
    }

    // The same triple again is a duplicate rather than a second rule.
    let repeated = authorize_params(&endpoint, network, Some(group), None);
    let (status, body) = call(&router, AUTHORIZE, &repeated).await;
    assert_eq!(
        status, 400,
        "a repeated triple is refused: got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_CLIENT_VPN_DUPLICATE_AUTHORIZATION_RULE,
        "a repeated triple answers the duplicate code, got {body}"
    );

    let networks = reported_networks(&router, "after the authorizes", &endpoint).await;
    assert_eq!(
        networks,
        vec![
            "10.1.0.0/16".to_string(),
            "10.1.0.0/16".to_string(),
            "10.1.0.0/16".to_string(),
            "10.2.0.0/16".to_string(),
        ],
        "four rules were authorized on this endpoint, got {networks:?}"
    );

    // A rule belongs to the endpoint it was added to.
    let other = reported_networks(&router, "the other endpoint", &other_endpoint).await;
    assert!(
        other.is_empty(),
        "the other endpoint holds no rule, got {other:?}"
    );
}

/// One row of the filter table: the case, the filters the report is narrowed by, and the
/// networks the rules it selects cover.
type FilterRow = (&'static str, Vec<(&'static str, &'static [&'static str])>, Vec<&'static str>);

/// One row of the refusal table: the case, the action, the form fields and the code EC2
/// answers.
type RefusalRow = (&'static str, &'static str, Vec<(String, String)>, &'static str);

/// Rule 2: each filter the operation's own documentation publishes narrows the report, and a
/// filter name it does not publish is refused rather than answered with an empty set.
#[tokio::test]
async fn every_documented_filter_narrows_the_report() {
    let (router, engine) = super::ec2_query::emulator();
    let endpoint = seed_endpoint(&engine, "the filtered endpoint");
    for params in [
        authorize_params(&endpoint, "10.1.0.0/16", Some("engineering"), Some("eng")),
        authorize_params(&endpoint, "10.2.0.0/16", Some("finance"), Some("fin")),
        authorize_params(&endpoint, "10.3.0.0/16", None, Some("eng")),
    ] {
        let (status, body) = call(&router, AUTHORIZE, &params).await;
        assert_eq!(status, 200, "seeding a rule: got {status} with body {body}");
    }

    // One row per filter name the model's own `Filters` documentation publishes.
    let cases: Vec<FilterRow> = vec![
        (
            "destination-cidr",
            vec![("destination-cidr", &["10.2.0.0/16"][..])],
            vec!["10.2.0.0/16"],
        ),
        (
            "group-id",
            vec![("group-id", &["engineering"][..])],
            vec!["10.1.0.0/16"],
        ),
        (
            "description",
            vec![("description", &["eng"][..])],
            vec!["10.1.0.0/16", "10.3.0.0/16"],
        ),
        (
            "two filters are joined with an AND",
            vec![
                ("description", &["eng"][..]),
                ("destination-cidr", &["10.3.0.0/16"][..]),
            ],
            vec!["10.3.0.0/16"],
        ),
        (
            "a value no rule carries selects nothing",
            vec![("group-id", &["nobody"][..])],
            Vec::new(),
        ),
    ];
    for (case, entries, expected) in cases {
        let mut params = endpoint_param(&endpoint);
        params.extend(filters(&entries));
        let (status, body) = call(&router, DESCRIBE, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {DESCRIBE}, got {status} with body {body}"
        );
        let mut reported = elements(&body, "destinationCidr");
        reported.sort();
        let mut wanted: Vec<String> = expected.iter().map(|value| (*value).to_string()).collect();
        wanted.sort();
        assert_eq!(
            reported, wanted,
            "case {case:?}: expected the rules the filter selects, got {body}"
        );
    }

    // A filter name the operation does not publish is refused.
    let mut params = endpoint_param(&endpoint);
    params.extend(filters(&[("tag-key", &["Name"][..])]));
    let (status, body) = call(&router, DESCRIBE, &params).await;
    assert_eq!(
        status, 400,
        "an undocumented filter is refused: got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidFilter",
        "an undocumented filter answers InvalidFilter, got {body}"
    );
}

/// Rule 2: the report is PAGED the way the model's own `NextToken`/`MaxResults` pair
/// describes — each page carries at most the page size, the token resumes where the page
/// ended, the last page mints none, a token minted for another endpoint does not resume this
/// one's report, and a page size outside the published range is refused.
#[tokio::test]
async fn the_report_pages_the_rules_of_the_endpoint_it_is_of() {
    let (router, engine) = super::ec2_query::emulator();
    let endpoint = seed_endpoint(&engine, "the paged endpoint");
    let other_endpoint = seed_endpoint(&engine, "another endpoint");
    // The smallest page the published range admits is 5, so the endpoint is given more rules
    // than one page can carry.
    let networks: Vec<String> = (0..7).map(|index| format!("10.{index}.0.0/16")).collect();
    for network in &networks {
        let (status, body) = call(
            &router,
            AUTHORIZE,
            &authorize_params(&endpoint, network, None, None),
        )
        .await;
        assert_eq!(status, 200, "seeding a rule: got {status} with body {body}");
    }
    let (status, body) = call(
        &router,
        AUTHORIZE,
        &authorize_params(&other_endpoint, "192.168.0.0/16", None, None),
    )
    .await;
    assert_eq!(
        status, 200,
        "seeding the other endpoint's rule: got {status} with body {body}"
    );

    // Page through the endpoint's rules at the published minimum page size, following each
    // token the report mints, and assert the pages reassemble its rules exactly once each.
    let mut paged: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for page in 0..networks.len() {
        let mut params = endpoint_param(&endpoint);
        params.push(p("MaxResults", "5"));
        if let Some(held) = token.clone() {
            params.push(p("NextToken", &held));
        }
        let (status, body) = call(&router, DESCRIBE, &params).await;
        assert_eq!(
            status, 200,
            "page {page}: expected HTTP 200 from {DESCRIBE}, got {status} with body {body}"
        );
        let reported = elements(&body, "destinationCidr");
        assert!(
            reported.len() <= 5,
            "page {page}: a page carries at most the page size, got {reported:?}"
        );
        paged.extend(reported);
        token = element(&body, "nextToken").filter(|minted| !minted.is_empty());
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        token, None,
        "the last page mints no token, got {token:?} after {paged:?}"
    );
    paged.sort();
    let mut expected = networks.clone();
    expected.sort();
    assert_eq!(
        paged, expected,
        "the pages reassemble the endpoint's rules exactly once each"
    );

    // A token is scoped to the endpoint whose report minted it.
    let mut params = endpoint_param(&endpoint);
    params.push(p("MaxResults", "5"));
    let (status, body) = call(&router, DESCRIBE, &params).await;
    assert_eq!(
        status, 200,
        "the first page: expected HTTP 200, got {status} with body {body}"
    );
    let minted = element(&body, "nextToken")
        .filter(|minted| !minted.is_empty())
        .expect("a seven-rule endpoint's first page of five mints a token");
    let mut params = endpoint_param(&other_endpoint);
    params.push(p("NextToken", &minted));
    let (status, body) = call(&router, DESCRIBE, &params).await;
    assert_eq!(
        status, 400,
        "a token minted for another endpoint is refused: got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidPaginationToken",
        "that refusal is the one this service answers for a token it did not mint here, got \
         {body}"
    );

    // A page size outside the published range, and a token this service never minted, are
    // each refused rather than served as some other page.
    let refusals: Vec<RefusalRow> = vec![
        (
            "a page size below the published minimum",
            DESCRIBE,
            vec![p("ClientVpnEndpointId", &endpoint), p("MaxResults", "4")],
            "InvalidMaxResults",
        ),
        (
            "a page size above the published maximum",
            DESCRIBE,
            vec![p("ClientVpnEndpointId", &endpoint), p("MaxResults", "1001")],
            "InvalidMaxResults",
        ),
        (
            "a token this service never minted",
            DESCRIBE,
            vec![
                p("ClientVpnEndpointId", &endpoint),
                p("NextToken", "not-a-token"),
            ],
            "InvalidPaginationToken",
        ),
    ];
    for (case, action, params, expected) in refusals {
        let (status, body) = call(&router, action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected the code {expected}, got {body}"
        );
    }
}

/// Rule 3: a revoke takes off exactly the rule it names, answers it `revoking`, and leaves
/// every other rule of the endpoint in place; one the endpoint does not have is refused.
#[tokio::test]
async fn a_revoke_takes_off_only_the_rule_it_names() {
    let (router, engine) = super::ec2_query::emulator();
    let endpoint = seed_endpoint(&engine, "the endpoint the rules are on");
    let network = "10.1.0.0/16";
    for params in [
        authorize_params(&endpoint, network, Some("engineering"), None),
        authorize_params(&endpoint, network, None, None),
        authorize_params(&endpoint, "10.2.0.0/16", Some("engineering"), None),
    ] {
        let (status, body) = call(&router, AUTHORIZE, &params).await;
        assert_eq!(status, 200, "seeding a rule: got {status} with body {body}");
    }

    // Revoking for ALL GROUPS "does not impact other authorization rules that allowed ingress
    // to the same TargetNetworkCidr with a specific AccessGroupId".
    let (status, body) = call(&router, REVOKE, &revoke_params(&endpoint, network, None)).await;
    assert_eq!(
        status, 200,
        "the all-groups revoke succeeds: got {status} with body {body}"
    );
    assert_eq!(
        element(&body, "code").as_deref(),
        Some(CLIENT_VPN_AUTHORIZATION_RULE_STATUS_REVOKING),
        "the revoke answers the rule revoking, got {body}"
    );
    let left = reported_networks(&router, "after the all-groups revoke", &endpoint).await;
    assert_eq!(
        left,
        vec!["10.1.0.0/16".to_string(), "10.2.0.0/16".to_string()],
        "the two group rules are left in place, got {left:?}"
    );

    // Then the one group's rule for that same network, which leaves the other network's.
    let (status, body) = call(
        &router,
        REVOKE,
        &revoke_params(&endpoint, network, Some("engineering")),
    )
    .await;
    assert_eq!(
        status, 200,
        "the group revoke succeeds: got {status} with body {body}"
    );
    let left = reported_networks(&router, "after the group revoke", &endpoint).await;
    assert_eq!(
        left,
        vec!["10.2.0.0/16".to_string()],
        "only the other network's rule is left, got {left:?}"
    );

    // A rule the endpoint no longer has is refused rather than reported as revoked.
    for (case, params) in [
        (
            "the rule just revoked",
            revoke_params(&endpoint, network, Some("engineering")),
        ),
        (
            "a network no rule covers",
            revoke_params(&endpoint, "10.9.0.0/16", Some("engineering")),
        ),
        (
            "a group that network is not granted to",
            revoke_params(&endpoint, "10.2.0.0/16", Some("finance")),
        ),
        (
            "all groups, where only one group's rule exists",
            revoke_params(&endpoint, "10.2.0.0/16", None),
        ),
    ] {
        let (status, body) = call(&router, REVOKE, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {REVOKE}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_CLIENT_VPN_ENDPOINT_AUTHORIZATION_RULE_NOT_FOUND,
            "case {case:?}: expected the rule-not-found code, got {body}"
        );
    }
}

/// Rule 1 and rule 3: the grant a request names has to be ONE grant — either a group or every
/// client — and the network has to be a network, so a request that names both, neither, or a
/// value that is no CIDR block at all is refused and nothing is stored.
#[tokio::test]
async fn a_request_that_names_no_single_grant_or_no_network_is_refused() {
    let (router, engine) = super::ec2_query::emulator();
    let endpoint = seed_endpoint(&engine, "the endpoint the rules are on");

    let cases: Vec<RefusalRow> = vec![
        (
            "both a group and all groups",
            AUTHORIZE,
            vec![
                p("ClientVpnEndpointId", &endpoint),
                p("TargetNetworkCidr", "10.1.0.0/16"),
                p("AccessGroupId", "engineering"),
                p("AuthorizeAllGroups", "true"),
            ],
            "InvalidParameterCombination",
        ),
        (
            "neither a group nor all groups",
            AUTHORIZE,
            vec![
                p("ClientVpnEndpointId", &endpoint),
                p("TargetNetworkCidr", "10.1.0.0/16"),
            ],
            "MissingParameter",
        ),
        (
            "no network at all",
            AUTHORIZE,
            vec![
                p("ClientVpnEndpointId", &endpoint),
                p("AuthorizeAllGroups", "true"),
            ],
            "MissingParameter",
        ),
        (
            "a value that is no CIDR block",
            AUTHORIZE,
            vec![
                p("ClientVpnEndpointId", &endpoint),
                p("TargetNetworkCidr", "10.1.0.0"),
                p("AuthorizeAllGroups", "true"),
            ],
            "InvalidParameterValue",
        ),
        (
            "a block whose host bits name an address inside a range",
            AUTHORIZE,
            vec![
                p("ClientVpnEndpointId", &endpoint),
                p("TargetNetworkCidr", "10.1.0.5/16"),
                p("AuthorizeAllGroups", "true"),
            ],
            "InvalidParameterValue",
        ),
        (
            "an endpoint the account does not hold",
            AUTHORIZE,
            authorize_params(ABSENT_ENDPOINT, "10.1.0.0/16", None, None),
            INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND,
        ),
        (
            "a revoke naming both a group and all groups",
            REVOKE,
            vec![
                p("ClientVpnEndpointId", &endpoint),
                p("TargetNetworkCidr", "10.1.0.0/16"),
                p("AccessGroupId", "engineering"),
                p("RevokeAllGroups", "true"),
            ],
            "InvalidParameterCombination",
        ),
        (
            "a revoke on an endpoint the account does not hold",
            REVOKE,
            revoke_params(ABSENT_ENDPOINT, "10.1.0.0/16", None),
            INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND,
        ),
        (
            "a report of an endpoint the account does not hold",
            DESCRIBE,
            endpoint_param(ABSENT_ENDPOINT),
            INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND,
        ),
    ];
    for (case, action, params, expected) in cases {
        let (status, body) = call(&router, action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected the code {expected}, got {body}"
        );
    }

    let stored = engine
        .state::<Ec2State>()
        .lock()
        .client_vpn_authorization_rules
        .len();
    assert_eq!(
        stored, 0,
        "no refusal stored a rule, got {stored} stored rules"
    );
}

/// Rule 1: one client token names the one rule it added, so a replay of the SAME request
/// reports that rule and adds nothing, while the token replayed with another description is
/// refused.
#[tokio::test]
async fn one_client_token_names_the_one_rule_it_added() {
    let (router, engine) = super::ec2_query::emulator();
    let endpoint = seed_endpoint(&engine, "the endpoint the rules are on");
    let mut params = authorize_params(&endpoint, "10.1.0.0/16", None, Some("the one rule"));
    params.push(p("ClientToken", "one-request"));

    for pass in ["the first authorize", "the replay"] {
        let (status, body) = call(&router, AUTHORIZE, &params).await;
        assert_eq!(
            status, 200,
            "case {pass:?}: expected HTTP 200, got {status} with body {body}"
        );
    }
    let networks = reported_networks(&router, "after the replay", &endpoint).await;
    assert_eq!(
        networks,
        vec!["10.1.0.0/16".to_string()],
        "the replay added no second rule, got {networks:?}"
    );

    let mut different = authorize_params(&endpoint, "10.1.0.0/16", None, Some("another rule"));
    different.push(p("ClientToken", "one-request"));
    let (status, body) = call(&router, AUTHORIZE, &different).await;
    assert_eq!(
        status, 400,
        "the token replayed with another description is refused: got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "IdempotentParameterMismatch",
        "the token replayed with another description answers the mismatch code, got {body}"
    );

    // A revoke drops the token that names the rule with the rule, so the index it is held in
    // cannot grow past the rules the store holds — and the token replays as a request for a
    // rule that is not there rather than naming one the store no longer holds.
    let (status, body) = call(
        &router,
        REVOKE,
        &revoke_params(&endpoint, "10.1.0.0/16", None),
    )
    .await;
    assert_eq!(
        status, 200,
        "the revoke succeeds: got {status} with body {body}"
    );
    assert!(
        engine
            .state::<Ec2State>()
            .lock()
            .client_vpn_authorization_rule_client_tokens
            .is_empty(),
        "the revoke forgot the token that named the rule"
    );
    let (status, body) = call(&router, AUTHORIZE, &params).await;
    assert_eq!(
        status, 200,
        "the same request after the revoke grants the rule again: got {status} with body {body}"
    );
    let networks = reported_networks(&router, "after the re-authorize", &endpoint).await;
    assert_eq!(
        networks,
        vec!["10.1.0.0/16".to_string()],
        "the re-authorize granted the one rule again, got {networks:?}"
    );
}

/// Rule 4: a delete answers the endpoint `deleting` and the clock owes it `deleted`, while an
/// endpoint that still has a target network associated is refused and stays as it was.
#[tokio::test]
async fn deleting_an_endpoint_needs_its_target_networks_disassociated_first() {
    let (router, engine) = super::ec2_query::emulator();
    let attached = seed_endpoint(&engine, "the endpoint with a target network");
    let association = seed_target_network(&engine, &attached);
    let detached = seed_endpoint(&engine, "the endpoint with no target network");
    // One grant on each endpoint: a rule is a grant ON an endpoint, so the rules of the one
    // that is taken down go with it while the other endpoint's stay.
    for endpoint in [&attached, &detached] {
        let (status, body) = call(
            &router,
            AUTHORIZE,
            &authorize_params(endpoint, "10.1.0.0/16", None, None),
        )
        .await;
        assert_eq!(status, 200, "seeding a rule: got {status} with body {body}");
    }

    let (status, body) = call(
        &router,
        DELETE_ENDPOINT,
        &[p("ClientVpnEndpointId", &attached)],
    )
    .await;
    assert_eq!(
        status, 400,
        "an endpoint with a target network cannot be deleted: got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DependencyViolation",
        "that refusal is EC2's DependencyViolation, got {body}"
    );
    assert!(
        body.contains(&association),
        "the refusal names the association that has to go first ({association}), got {body}"
    );

    let (status, body) = call(
        &router,
        DELETE_ENDPOINT,
        &[p("ClientVpnEndpointId", &detached)],
    )
    .await;
    assert_eq!(
        status, 200,
        "an endpoint with no target network is deleted: got {status} with body {body}"
    );
    assert_eq!(
        element(&body, "code").as_deref(),
        Some(CLIENT_VPN_ENDPOINT_STATUS_DELETING),
        "the delete answers the endpoint deleting, got {body}"
    );

    // The deleted endpoint's grants went with it, and the other endpoint's did not.
    let left = engine
        .state::<Ec2State>()
        .lock()
        .client_vpn_authorization_rules
        .keys()
        .map(|key| key.client_vpn_endpoint_id.clone())
        .collect::<Vec<String>>();
    assert_eq!(
        left,
        vec![attached.clone()],
        "only the endpoint that was not deleted still grants its network, got {left:?}"
    );

    // The move the clock owes the endpoint, looked for until it holds.
    let deadline = Instant::now() + POLL_DEADLINE;
    let settled = loop {
        let (status, body) = call(
            &router,
            "DescribeClientVpnEndpoints",
            &[p("ClientVpnEndpointId.1", &detached)],
        )
        .await;
        assert_eq!(
            status, 200,
            "the deleted endpoint is still reported: got {status} with body {body}"
        );
        let held = element(&body, "code");
        if held.as_deref() == Some(CLIENT_VPN_ENDPOINT_STATUS_DELETED)
            || Instant::now() >= deadline
        {
            break held;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    };
    assert_eq!(
        settled.as_deref(),
        Some(CLIENT_VPN_ENDPOINT_STATUS_DELETED),
        "the clock owes the endpoint deleted, got {settled:?}"
    );

    // An endpoint the account never held is refused rather than reported as deleted.
    let (status, body) = call(
        &router,
        DELETE_ENDPOINT,
        &[p("ClientVpnEndpointId", ABSENT_ENDPOINT)],
    )
    .await;
    assert_eq!(
        status, 400,
        "an endpoint the account does not hold is refused: got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND,
        "that refusal is the one this surface answers for an endpoint id, got {body}"
    );
}
