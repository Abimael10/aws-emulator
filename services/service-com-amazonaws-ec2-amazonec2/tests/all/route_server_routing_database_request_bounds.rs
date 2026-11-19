// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what bounds ONE `GetRouteServerRoutingDatabase` request, driven over
//! the wire the service speaks — `ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness.
//!
//! The rule under test: selecting entries out of a route server's routing database costs
//! filters x values x value length x entries, and the model bounds none of those factors
//! (`FilterList` and `Filter$Values` carry no `@length`), so each request-controlled factor is
//! capped by the crate's shared ceiling — served at that ceiling, refused one past it with
//! `InvalidParameterValue` and the wording this crate's every filtered report refuses it with,
//! and refused BEFORE the store is read and before a `DryRun` is answered, so what a request
//! costs is decided by the request alone. The one bound that is not the request's own cost,
//! the `@required` `RouteServerId` the request must carry, is still answered first.
//!
//! Each table row is a distinct input along what that rule generalises over: each of the three
//! capped factors (the filter count, one filter's value count, one value's length), each at its
//! published bound and one past it, and each ordering pair of a capped request with a condition
//! the handler checks before or after the caps.
//!
//! What this report answers for a request that is WITHIN every bound — the database of a route
//! server whose sessions carried nothing, the refusal of an id that resolves to nothing, the
//! filter names it answers, and that reading writes nothing — is the business of
//! [`super::route_server_routing_database`];
//! the baseline rows below exist only as the contrast the ordering claim is read against, so
//! anything they assert is asserted there too.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{
    DRY_RUN_OPERATION, DRY_RUN_OPERATION_STATUS, INVALID_PARAMETER_VALUE,
    INVALID_ROUTE_SERVER_ID_NOT_FOUND, MAX_FILTER_VALUE_BYTES, MAX_FILTER_VALUES, MAX_FILTERS,
    MISSING_PARAMETER,
};

use super::ec2_query::{call, element, elements, error_code, error_message, filters};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// A filter name this operation answers (`RouteServerRoute$Prefix` under its kebab-cased
/// name), so every row below is refused for its SIZE rather than for naming an attribute the
/// operation does not filter on.
const DOCUMENTED_FILTER: &str = "prefix";

/// One request's form parameters.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter, as the ec2Query harness takes them.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// A route server with `asn` as its Amazon-side ASN, and the id it was minted with.
async fn create_route_server(case: &str, router: &MultiServiceRouter, asn: i64) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateRouteServer",
        &[p("AmazonSideAsn", asn.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateRouteServer, got {status} with body {body}"
    );
    element(&body, "routeServerId")
        .unwrap_or_else(|| panic!("case {case:?}: no routeServerId in {body}"))
}

/// One `GetRouteServerRoutingDatabase` call carrying exactly `params`.
async fn database(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "GetRouteServerRoutingDatabase", params).await
}

/// `params` naming `route_server_id`, with `extra` beside it.
fn of(route_server_id: &str, extra: Params) -> Params {
    let mut params = vec![p("RouteServerId", route_server_id)];
    params.extend(extra);
    params
}

/// `count` filters, every one of them a name this operation answers.
fn many_filters(count: usize) -> Params {
    let entries: Vec<(&str, &[&str])> = vec![(DOCUMENTED_FILTER, &["10.0.0.0/16"][..]); count];
    filters(&entries)
}

/// One documented filter carrying `count` values.
fn many_values(count: usize) -> Params {
    let values: Vec<String> = (0..count).map(|index| format!("10.0.{index}.0/24")).collect();
    let borrowed: Vec<&str> = values.iter().map(String::as_str).collect();
    filters(&[(DOCUMENTED_FILTER, &borrowed)])
}

/// One documented filter carrying a single value of `bytes` bytes.
fn value_of_length(bytes: usize) -> Params {
    let value = "a".repeat(bytes);
    filters(&[(DOCUMENTED_FILTER, &[value.as_str()])])
}

/// The prefixes a report named — none, for a route server whose sessions carried no
/// advertisement, which is every route server of this emulator.
fn reported_prefixes(body: &str) -> Vec<String> {
    elements(body, "prefix")
}

/// A request at each published bound is SERVED: the ceilings refuse what is past them, never
/// the bound itself, and a database that holds no entry selects none for any of them.
#[tokio::test]
async fn a_request_at_each_published_bound_is_served() {
    let router = emulator();
    let case = "the route server read at the bounds";
    let route_server_id = create_route_server(case, &router, 64_512).await;

    // (case, the request's members beside its RouteServerId)
    let rows: Vec<(String, Params)> = vec![
        ("no filter at all".to_string(), Vec::new()),
        (
            format!("{MAX_FILTERS} filters, the bound itself"),
            many_filters(MAX_FILTERS),
        ),
        (
            format!("{MAX_FILTER_VALUES} values of one filter, the bound itself"),
            many_values(MAX_FILTER_VALUES),
        ),
        (
            format!("a value of {MAX_FILTER_VALUE_BYTES} bytes, the bound itself"),
            value_of_length(MAX_FILTER_VALUE_BYTES),
        ),
    ];

    for (case, extra) in rows {
        let (status, body) = database(&router, &of(&route_server_id, extra)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let prefixes = reported_prefixes(&body);
        assert!(
            prefixes.is_empty(),
            "case {case:?}: expected no entry from a database that holds none, got {prefixes:?} \
             in {body}"
        );
        let token = element(&body, "nextToken");
        assert_eq!(
            token, None,
            "case {case:?}: expected no NextToken on a page nothing follows, got {token:?}"
        );
        let persisted = element(&body, "areRoutesPersisted");
        assert_eq!(
            persisted.as_deref(),
            Some("false"),
            "case {case:?}: a route server created without route persistence persists none, \
             got {persisted:?} in {body}"
        );
    }
}

/// A request one past ANY of the three capped factors is refused, with the code and the
/// wording this crate refuses an oversized filter list with.
#[tokio::test]
async fn a_request_past_a_published_bound_is_refused() {
    let router = emulator();
    let case = "the route server read past the bounds";
    let route_server_id = create_route_server(case, &router, 64_513).await;

    // (case, the request's members beside its RouteServerId, the message)
    let rows: Vec<(String, Params, String)> = vec![
        (
            format!("{} filters, one past the bound", MAX_FILTERS + 1),
            many_filters(MAX_FILTERS + 1),
            format!(
                "The request carries {} filters, above the maximum of {MAX_FILTERS}",
                MAX_FILTERS + 1
            ),
        ),
        (
            format!("{} values of one filter, one past the bound", MAX_FILTER_VALUES + 1),
            many_values(MAX_FILTER_VALUES + 1),
            format!(
                "The filter '{DOCUMENTED_FILTER}' carries {} values, above the maximum of \
                 {MAX_FILTER_VALUES}",
                MAX_FILTER_VALUES + 1
            ),
        ),
        (
            format!(
                "a value of {} bytes, one past the bound",
                MAX_FILTER_VALUE_BYTES + 1
            ),
            value_of_length(MAX_FILTER_VALUE_BYTES + 1),
            format!(
                "A value of the filter '{DOCUMENTED_FILTER}' carries {} bytes, above the \
                 maximum of {MAX_FILTER_VALUE_BYTES}",
                MAX_FILTER_VALUE_BYTES + 1
            ),
        ),
    ];

    for (case, extra, message) in rows {
        let (status, body) = database(&router, &of(&route_server_id, extra)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected the error code {INVALID_PARAMETER_VALUE:?}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            message,
            "case {case:?}: expected the message {message:?}, got {body}"
        );
    }
}

/// What a request COSTS is decided before the store is read and before a dry run is answered,
/// while the `@required` `RouteServerId` a request did not carry is answered ahead of
/// everything — each ordering asserted beside the refusal the same condition gets on its own,
/// so the rows pin the ORDER rather than one refusal.
#[tokio::test]
async fn a_requests_own_bounds_are_answered_before_the_store_is_read() {
    let router = emulator();
    let held = create_route_server("the route server the account holds", &router, 64_514).await;
    // A well-formed route server id of the long published form that names no route server of
    // the account: the store would refuse it, so a row that still answers the filter refusal
    // shows the store was never reached.
    let unknown = "rs-0a1b2c3d4e5f60718";

    // (case, the whole request beside its Action, the code)
    let rows: Vec<(&str, Params, &str)> = vec![
        (
            "an oversized filter list against a route server the account does not hold",
            of(unknown, many_filters(MAX_FILTERS + 1)),
            INVALID_PARAMETER_VALUE,
        ),
        (
            "an oversized filter list on a dry run",
            {
                let mut params = of(&held, many_filters(MAX_FILTERS + 1));
                params.push(p("DryRun", "true"));
                params
            },
            INVALID_PARAMETER_VALUE,
        ),
        (
            "an oversized filter list with no RouteServerId at all",
            many_filters(MAX_FILTERS + 1),
            MISSING_PARAMETER,
        ),
        (
            "no RouteServerId at all",
            Vec::new(),
            MISSING_PARAMETER,
        ),
        (
            "a route server the account does not hold, within every bound",
            of(unknown, many_filters(MAX_FILTERS)),
            INVALID_ROUTE_SERVER_ID_NOT_FOUND,
        ),
    ];

    for (case, params, code) in rows {
        let (status, body) = database(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {case:?}: expected the error code {code:?}, got {body}"
        );
    }

    // The contrast for the dry run row above: a dry run of a request that is within every
    // bound and names a route server the account holds reports that outcome instead.
    let (status, body) = database(&router, &of(&held, vec![p("DryRun", "true")])).await;
    assert_eq!(
        status, DRY_RUN_OPERATION_STATUS,
        "a dry run of a read that would have succeeded reports that outcome, got {status} with \
         body {body}"
    );
    assert_eq!(
        error_code(&body),
        DRY_RUN_OPERATION,
        "a dry run of a read that would have succeeded is {DRY_RUN_OPERATION}, got {body}"
    );
}
