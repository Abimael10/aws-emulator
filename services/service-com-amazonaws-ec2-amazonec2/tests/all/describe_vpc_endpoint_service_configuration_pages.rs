// SPDX-License-Identifier: CC0-1.0

//! How `DescribeVpcEndpointServiceConfigurations` SELECTS and PAGES the endpoint services the
//! account configured, driven over `ec2Query` (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: the describe reports the configurations the account's own creates
//! stored, narrowed by `ServiceIds` and by the documented filters it joins — the values of one
//! filter with an OR and matched with EC2's `*`/`?` wildcards, several filters with an AND —
//! and walks that selection in `ServiceId` order in pages of at most the requested size,
//! naming the next page with a token exactly while a configuration follows the page it
//! answered; a filter name it does not document is refused rather than read as "no results",
//! and no describe, served or refused, changes what a later describe reports.
//!
//! Each row below is a distinct input along what that rule generalises over: the requested page
//! size, from the smallest page the operation serves through the largest to one past it (which
//! is served rather than refused); whether a filter carries one value or several; whether a
//! value is literal or a wildcard; how many filters have to hold at once; and whether
//! `ServiceIds` names a service once or twice. The suite beside this one covers what a create
//! stores and what one describe reports of a single service, and `published_parameter_limits`
//! covers the refusal of a page size below the published minimum.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, filters, indexed};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The largest page this operation serves, and the largest page size a request may name
/// without being refused: "This value can be between 5 and 1,000; if MaxResults is given a
/// value larger than 1,000, only 1,000 results are returned"
/// (`DescribeVpcEndpointServiceConfigurationsRequest$MaxResults`).
const MAX_PAGE_SIZE: i32 = 1000;

/// The smallest page this operation serves, from that same sentence.
const MIN_PAGE_SIZE: i32 = 5;

/// How many services every test here configures: one more than [`MIN_PAGE_SIZE`] plus one, so
/// the smallest page this operation serves cannot hold them all and a walk of that size takes
/// more than two pages.
const CONFIGURED: usize = 7;

/// The index of the service the setup tags, and of the one it offers over IPv6 — two distinct
/// services, so a filter on either selects exactly one of the configured services.
const TAGGED: usize = 2;
const IPV6: usize = 4;

/// The emulator the tests drive. The store is reached only over the wire here, so the engine
/// handle is not needed.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The ARN of the Network Load Balancer `name`, as Elastic Load Balancing writes one.
///
/// A load balancer is a resource of another service, so this is only ever a well-formed ARN
/// here: what the create stores is the ARN the request named.
fn load_balancer_arn(name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/net/{name}/0123456789abcdef",
        account = engine::emulator_account_id(),
    )
}

/// Configure [`CONFIGURED`] endpoint services through the API, one of them tagged and one of
/// them offered over IPv6, and answer their `ServiceId`s in the order the describe reports
/// them (`ServiceId` order).
async fn configure_services(case: &str, router: &MultiServiceRouter) -> Vec<String> {
    let mut ids = Vec::with_capacity(CONFIGURED);
    for index in 0..CONFIGURED {
        let arn = load_balancer_arn(&format!("balancer-{index}"));
        let mut params = vec![param("NetworkLoadBalancerArn.1", &arn)];
        if index == TAGGED {
            params.push(param("TagSpecification.1.ResourceType", "vpc-endpoint-service"));
            params.push(param("TagSpecification.1.Tag.1.Key", "Stage"));
            params.push(param("TagSpecification.1.Tag.1.Value", "canary"));
        }
        if index == IPV6 {
            params.push(param("SupportedIpAddressType.1", "ipv6"));
        }
        let body = ok(
            case,
            router,
            "CreateVpcEndpointServiceConfiguration",
            &params,
        )
        .await;
        let id = element(&body, "serviceId")
            .unwrap_or_else(|| panic!("case {case:?}: no <serviceId> in {body}"));
        ids.push(id);
    }
    ids.sort();
    ids
}

/// The `serviceId`s one `DescribeVpcEndpointServiceConfigurations` reports for `params`, and
/// the `nextToken` it answered with.
async fn describe(
    case: &str,
    router: &MultiServiceRouter,
    params: &[Param],
) -> (Vec<String>, Option<String>) {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpointServiceConfigurations",
        params,
    )
    .await;
    (elements(&body, "serviceId"), element(&body, "nextToken"))
}

/// The `serviceName` the describe reports for the one service `service_id` names.
async fn service_name(case: &str, router: &MultiServiceRouter, service_id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpointServiceConfigurations",
        &indexed("ServiceId", &[service_id]),
    )
    .await;
    element(&body, "serviceName")
        .unwrap_or_else(|| panic!("case {case:?}: no <serviceName> in {body}"))
}

/// Walk every page of a describe that names the page size `requested`, and answer the
/// `ServiceId`s the whole walk reported, in the order the pages reported them.
///
/// Each page is checked as it arrives: it holds at most the page size the request may have
/// (a request naming more than [`MAX_PAGE_SIZE`] gets that largest page), it is never empty,
/// and it carries a token exactly while configurations are still to come.
async fn walk(
    case: &str,
    router: &MultiServiceRouter,
    requested: Option<i32>,
    total: usize,
) -> Vec<String> {
    let served = requested
        .map(|size| size.min(MAX_PAGE_SIZE))
        .unwrap_or(MAX_PAGE_SIZE)
        .max(1) as usize;
    let mut reported: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // A walk of `total` rows takes at most `total` pages, since no page is empty; the extra
    // round is what would let an over-long walk show up as a failure here rather than as a
    // loop that never ends.
    for _page in 0..=total {
        let mut params: Vec<Param> = Vec::new();
        if let Some(size) = requested {
            params.push(param("MaxResults", &size.to_string()));
        }
        if let Some(cursor) = &token {
            params.push(param("NextToken", cursor));
        }
        let (page, next) = describe(case, router, &params).await;
        assert!(
            !page.is_empty(),
            "case {case:?}: a page of a walk that is not finished must report at least one \
             configuration, got none after {reported:?}"
        );
        assert!(
            page.len() <= served,
            "case {case:?}: expected a page of at most {served} configurations, got {}",
            page.len()
        );
        reported.extend(page);
        token = next;
        match &token {
            Some(cursor) => assert!(
                reported.len() < total,
                "case {case:?}: expected no next-page token once all {total} configurations \
                 were reported, got {cursor:?} after {reported:?}"
            ),
            None => break,
        }
    }
    assert!(
        token.is_none(),
        "case {case:?}: expected the walk to end without a token, got {token:?} after \
         {reported:?}"
    );
    reported
}

/// A page of any size the operation serves walks the whole store once, in `ServiceId` order:
/// the smallest page it serves, one larger, exactly as many as are stored, the largest page it
/// serves, one past that largest, and no page size at all.
#[tokio::test]
async fn pages_of_every_served_size_walk_the_stored_configurations_once() {
    let router = emulator();
    let ids = configure_services("the paging table", &router).await;

    let rows: Vec<(&str, Option<i32>)> = vec![
        ("the smallest page this operation serves", Some(MIN_PAGE_SIZE)),
        ("one more than that smallest page", Some(MIN_PAGE_SIZE + 1)),
        ("a page as large as the store", Some(CONFIGURED as i32)),
        ("the largest page this operation serves", Some(MAX_PAGE_SIZE)),
        ("a page larger than the largest served", Some(MAX_PAGE_SIZE + 1)),
        ("no page size at all", None),
    ];
    for (case, requested) in rows {
        let reported = walk(case, &router, requested, ids.len()).await;
        assert_eq!(
            reported, ids,
            "case {case:?}: expected the whole store once, in ServiceId order"
        );
    }
}

/// What a filter selects: one value, several values joined with an OR, a wildcard value, and
/// several filters that all have to hold.
#[tokio::test]
async fn filter_values_are_joined_with_an_or_and_filters_with_an_and() {
    let router = emulator();
    let ids = configure_services("the filter table", &router).await;
    let tagged = ids[TAGGED].clone();
    let over_ipv6 = ids[IPV6].clone();
    let tagged_name = service_name("the name of the tagged service", &router, &tagged).await;
    // The names of the account's own services share every label but the last, which is the
    // service's own id — so a wildcard over that last label selects every one of them, and one
    // over everything BEFORE that last label selects exactly the service it came from. Both
    // patterns are built out of the name the reply carried rather than assembled from the shape
    // of a name, so this suite pins the JOINING of filter values and not that shape.
    let (prefix, last_label) = tagged_name
        .rsplit_once('.')
        .unwrap_or_else(|| panic!("the reported service name {tagged_name:?} carries no label"));
    let family = format!("{prefix}.*");
    let tail = format!("*{last_label}");

    let rows: Vec<(&str, Vec<Param>, Vec<String>)> = vec![
        (
            "one value of the service-id filter",
            filters(&[("service-id", &[tagged.as_str()])]),
            vec![tagged.clone()],
        ),
        (
            "three values of the service-id filter, any of which may match",
            filters(&[(
                "service-id",
                &[ids[0].as_str(), tagged.as_str(), over_ipv6.as_str()],
            )]),
            vec![ids[0].clone(), tagged.clone(), over_ipv6.clone()],
        ),
        (
            "a value of the service-id filter that names no service of the account",
            filters(&[("service-id", &["vpce-svc-0123456789abcdef0"])]),
            Vec::new(),
        ),
        (
            "one of several values naming no service, the other naming one",
            filters(&[(
                "service-id",
                &["vpce-svc-0123456789abcdef0", tagged.as_str()],
            )]),
            vec![tagged.clone()],
        ),
        (
            "a wildcard over the last label of every name",
            filters(&[("service-name", &[family.as_str()])]),
            ids.clone(),
        ),
        (
            "a wildcard matching the tail of one name only",
            filters(&[("service-name", &[tail.as_str()])]),
            vec![tagged.clone()],
        ),
        (
            "a single-character wildcard inside a service-state",
            filters(&[("service-state", &["Availabl?"])]),
            ids.clone(),
        ),
        (
            "two filters that both hold for one service",
            filters(&[("tag-key", &["Stage"]), ("service-state", &["Available"])]),
            vec![tagged.clone()],
        ),
        (
            "three filters that all hold for one service",
            filters(&[
                ("tag:Stage", &["canary"]),
                ("service-id", &[tagged.as_str()]),
                ("supported-ip-address-types", &["ipv4"]),
            ]),
            vec![tagged.clone()],
        ),
        (
            "three filters one of which holds for no service",
            filters(&[
                ("tag:Stage", &["canary"]),
                ("service-id", &[tagged.as_str()]),
                ("supported-ip-address-types", &["ipv6"]),
            ]),
            Vec::new(),
        ),
        (
            "a tag value no service carries",
            filters(&[("tag:Stage", &["production"])]),
            Vec::new(),
        ),
    ];
    for (case, params, expected) in rows {
        let (reported, token) = describe(case, &router, &params).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected exactly those services"
        );
        assert_eq!(
            token, None,
            "case {case:?}: a selection that fits one page names no next page, got {token:?}"
        );
    }
}

/// A filter name is case-sensitive, so a documented name in another case is refused — and the
/// refusal leaves every configuration reportable.
#[tokio::test]
async fn a_documented_filter_name_in_another_case_is_refused() {
    let router = emulator();
    let ids = configure_services("the filter-name case table", &router).await;

    let rows = [
        ("the first letter capitalized", "Service-id"),
        ("every letter capitalized", "SERVICE-ID"),
        ("an underscore for the hyphen", "service_id"),
        ("a tag filter named in another case", "Tag:Stage"),
    ];
    for (case, name) in rows {
        let (status, body) = call(
            &router,
            "DescribeVpcEndpointServiceConfigurations",
            &filters(&[(name, &[ids[TAGGED].as_str()])]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for the filter {name:?}, got {status} with body \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {case:?}: expected the parameter refusal for the filter {name:?}, got {body}"
        );
    }
    let (reported, _) = describe("after the filter-name refusals", &router, &[]).await;
    assert_eq!(
        reported, ids,
        "a refused filter name must leave every configuration reportable"
    );
}

/// `ServiceIds` narrows the describe to the services it names, however many times it names
/// one — and a repeated describe reports the same services, a read changing nothing.
#[tokio::test]
async fn service_ids_narrow_the_describe_and_a_repeat_reports_the_same() {
    let router = emulator();
    let ids = configure_services("the ServiceIds table", &router).await;

    let rows: Vec<(&str, Vec<&str>, Vec<String>)> = vec![
        (
            "one id",
            vec![ids[1].as_str()],
            vec![ids[1].clone()],
        ),
        (
            "the same id twice",
            vec![ids[1].as_str(), ids[1].as_str()],
            vec![ids[1].clone()],
        ),
        (
            "two ids, the second given twice",
            vec![ids[0].as_str(), ids[1].as_str(), ids[1].as_str()],
            vec![ids[0].clone(), ids[1].clone()],
        ),
        (
            "every id, out of ServiceId order",
            ids.iter().rev().map(String::as_str).collect(),
            ids.clone(),
        ),
    ];
    for (case, named, expected) in rows {
        let params = indexed("ServiceId", &named);
        let (first, _) = describe(case, &router, &params).await;
        assert_eq!(
            first, expected,
            "case {case:?}: expected exactly those services, in ServiceId order"
        );
        let (again, _) = describe(case, &router, &params).await;
        assert_eq!(
            again, first,
            "case {case:?}: a repeated describe must report what the first one did"
        );
    }

    // The ids narrow the walk the pages run over too: a page smaller than the number of ids
    // named still reports only those ids, once each.
    let case = "a page smaller than the number of ids named";
    let named: Vec<&str> = ids.iter().map(String::as_str).collect();
    let mut params = indexed("ServiceId", &named);
    params.push(param("MaxResults", &MIN_PAGE_SIZE.to_string()));
    let (page, token) = describe(case, &router, &params).await;
    assert_eq!(
        page,
        ids[..MIN_PAGE_SIZE as usize].to_vec(),
        "case {case:?}: expected the first page of the named ids"
    );
    let cursor =
        token.unwrap_or_else(|| panic!("case {case:?}: expected a token naming the next page"));
    params.push(param("NextToken", &cursor));
    let (rest, token) = describe(case, &router, &params).await;
    assert_eq!(
        rest,
        ids[MIN_PAGE_SIZE as usize..].to_vec(),
        "case {case:?}: expected the rest of the named ids"
    );
    assert_eq!(
        token, None,
        "case {case:?}: the last page names no next page, got {token:?}"
    );
}
