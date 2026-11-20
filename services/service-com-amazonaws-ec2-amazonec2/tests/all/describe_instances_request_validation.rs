// SPDX-License-Identifier: CC0-1.0

//! How `DescribeInstances` VALIDATES its request, end to end over the wire the service
//! speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML —
//! README.md / OPERATIONS.md "Wire protocol"), through the shared [`super::ec2_query`]
//! harness.
//!
//! Three rules, each asserted over a TABLE whose rows differ along what the rule
//! generalises over rather than along the one request that exposed it:
//!
//! 1. A FILTER NAME the operation does not document is refused — `InvalidParameterValue`
//!    with "The filter '<name>' is invalid", the name taken from the request — while a
//!    documented name whose VALUE selects nothing is served: a 200 reporting no
//!    reservation. So a near miss of a documented name is a refusal and an unmatched value
//!    is an empty answer, which are different outcomes for different mistakes.
//! 2. A `MaxResults` below the smallest page the operation pages with is refused —
//!    `InvalidParameterValue` with "Value ( <n> ) for parameter maxResults is invalid.
//!    Expecting a value greater than <minimum>.", the value from the request and the
//!    minimum the operation publishes — whatever else the request selects, while the
//!    minimum itself and anything above it is served.
//! 3. An explicit id selection beside a page size is refused —
//!    `InvalidParameterCombination` with "The parameter instancesSet cannot be used with
//!    the parameter maxResults", naming the two parameters as the ec2Query WIRE spells
//!    them — whatever the ids and whatever the page size; either parameter on its own is
//!    served.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements, error_code, error_message, filters};

/// The Region every call here is signed for: `DescribeInstances` reports the instances of
/// the Region its request was signed for, and every instance below is launched in this one.
const REGION: &str = "us-east-1";

/// The image and the instance type every launch below names, so a row can filter on either
/// by a value the launch really stored.
const AMI: &str = "ami-0123456789abcdef0";
const INSTANCE_TYPE: &str = "t3.nano";

/// The tag one launch below carries, so a row can select exactly the instances it launched
/// the way a caller sweeping its own resources does.
const PROBE_TAG_KEY: &str = "describe-instances-validation";
const PROBE_TAG_VALUE: &str = "one";

/// The smallest page `DescribeInstances` pages with.
///
/// The model puts no `@range` on `DescribeInstancesRequest$MaxResults`
/// (`smithy_query(query="operation",operation="DescribeInstances")`), so the bound is the
/// one AWS publishes for the member in the EC2 API Reference ("Valid Range: Minimum value
/// of 5. Maximum value of 1000.").
const PUBLISHED_PAGE_SIZE_MINIMUM: i32 = 5;

/// The largest page the operation serves, the other end of that published range.
const PUBLISHED_PAGE_SIZE_MAXIMUM: i32 = 1_000;

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One call, as its HTTP status and body.
async fn op(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call that must succeed, as its body.
async fn ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = op(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Assert one refusal: HTTP 400, EC2's code, and the exact sentence it carries.
async fn refused(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
    expected_code: &str,
    expected_message: &str,
) {
    let (status, body) = op(router, "DescribeInstances", params).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from DescribeInstances, got {status} with body \
         {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, expected_code,
        "case {case:?}: expected the code {expected_code:?}, got {code:?} in {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message, expected_message,
        "case {case:?}: expected the message {expected_message:?}, got {message:?}"
    );
}

/// EC2's sentence for a filter name the operation does not document.
fn invalid_filter_message(name: &str) -> String {
    format!("The filter '{name}' is invalid")
}

/// EC2's sentence for a page size below the minimum this operation publishes: the value the
/// request sent, in spaced parentheses, and that minimum. Written here rather than read from
/// the service, so the suite pins the WIRE text.
fn below_minimum_message(value: i32) -> String {
    format!(
        "Value ( {value} ) for parameter maxResults is invalid. \
         Expecting a value greater than {PUBLISHED_PAGE_SIZE_MINIMUM}."
    )
}

/// EC2's sentence for an id selection beside a page size, naming both parameters as the
/// ec2Query wire spells them: the id list's set name and the page size's member name.
const ID_SELECTION_WITH_PAGE_SIZE_MESSAGE: &str =
    "The parameter instancesSet cannot be used with the parameter maxResults";

/// Launch `count` instances in one reservation, tagged so a filter can select exactly them,
/// and answer their ids.
async fn launch(router: &MultiServiceRouter, case: &str, count: u32) -> Vec<String> {
    let mut params = vec![
        p("ImageId", AMI),
        p("InstanceType", INSTANCE_TYPE),
        p("MinCount", count.to_string()),
        p("MaxCount", count.to_string()),
    ];
    params.extend(super::ec2_query::tag_specification(
        "instance",
        &[(PROBE_TAG_KEY, PROBE_TAG_VALUE)],
    ));
    let body = ok(router, case, "RunInstances", &params).await;
    let ids = elements(&body, "instanceId");
    assert_eq!(
        ids.len(),
        count as usize,
        "case {case:?}: expected {count} instance id(s) from RunInstances, got {ids:?}"
    );
    ids
}

/// The `tag:<key>` filter name a caller sweeping its own instances sends.
fn probe_filter_name() -> String {
    format!("tag:{PROBE_TAG_KEY}")
}

/// The tag filter a caller sweeping its own instances sends: the one the rows below select
/// the launched instances with.
fn probe_filter() -> Vec<(String, String)> {
    filters(&[(probe_filter_name().as_str(), &[PROBE_TAG_VALUE][..])])
}

// ---------------------------------------------------------------------------
// 1. The filter vocabulary
// ---------------------------------------------------------------------------

/// A filter NAME the operation does not document is refused with EC2's own sentence, and a
/// documented name whose VALUE matches nothing is served as a 200 reporting no reservation.
#[tokio::test]
async fn an_undocumented_filter_name_is_refused_and_an_unmatched_value_is_an_empty_answer() {
    let (router, _engine) = super::ec2_query::emulator();
    let launched = launch(&router, "seeding one instance", 1).await;

    // Rows: names that are NOT in the documented catalog, differing in HOW they miss it — a
    // near miss of a documented name (the family name without its `-name` suffix, a plural,
    // a different separator, a different case), the `tag:` prefix with no key after it, and
    // a name of no documented family at all. Source of the catalog these miss: the `Filters`
    // member documentation of the request shape
    // (`smithy_query(query="doc",shape="DescribeInstancesRequest",member="Filters")`).
    for name in [
        "instance-state",
        "instance-states",
        "instance-types",
        "instance_state_name",
        "Instance-Type",
        "tag:",
        "colour",
    ] {
        let case = format!("a filter named {name:?}");
        refused(
            &router,
            &case,
            &filters(&[(name, &["running"][..])]),
            "InvalidParameterValue",
            &invalid_filter_message(name),
        )
        .await;
        // The same name beside a filter that DOES select: an undocumented name is refused
        // however the rest of the request would have narrowed the answer.
        let selecting = probe_filter_name();
        let mixed = filters(&[
            (selecting.as_str(), &[PROBE_TAG_VALUE][..]),
            (name, &["running"][..]),
        ]);
        refused(
            &router,
            &format!("{case} beside a tag filter that selects"),
            &mixed,
            "InvalidParameterValue",
            &invalid_filter_message(name),
        )
        .await;
    }

    // Rows: documented names whose value selects NOTHING — a state no launched instance is
    // in, another image, another instance type, a tag key the account never wrote, and a key
    // pair name no launch named. Each is a 200 with no reservation, not a refusal.
    for (name, value) in [
        ("instance-state-name", "terminated"),
        ("instance-state-code", "48"),
        ("image-id", "ami-00000000000000000"),
        ("instance-type", "m5.24xlarge"),
        ("tag:no-such-key", PROBE_TAG_VALUE),
        ("key-name", "no-such-key-pair"),
    ] {
        let case = format!("the documented filter {name:?} with the value {value:?}");
        let body = ok(
            &router,
            &case,
            "DescribeInstances",
            &filters(&[(name, &[value][..])]),
        )
        .await;
        let reported = elements(&body, "reservationId");
        assert!(
            reported.is_empty(),
            "case {case:?}: a documented filter nothing matches reports no reservation, got \
             {reported:?}"
        );
    }

    // Rows: documented names whose value DOES select, so the catalog entries above are
    // resolved rather than merely accepted.
    for (name, value) in [
        ("instance-id", launched[0].as_str()),
        ("image-id", AMI),
        ("instance-type", INSTANCE_TYPE),
        ("tag-key", PROBE_TAG_KEY),
    ] {
        let case = format!("the documented filter {name:?} with the value {value:?}");
        let body = ok(
            &router,
            &case,
            "DescribeInstances",
            &filters(&[(name, &[value][..])]),
        )
        .await;
        let reported = elements(&body, "instanceId");
        assert_eq!(
            reported, launched,
            "case {case:?}: expected the launched instance {launched:?}, got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The page-size floor
// ---------------------------------------------------------------------------

/// A page size below the minimum the operation publishes is refused with EC2's sentence
/// carrying the value the request sent, whatever else the request selects; the minimum and
/// every size above it is served, and still finds what the request selected.
#[tokio::test]
async fn a_page_size_below_the_published_minimum_is_refused_however_the_request_selects() {
    let (router, _engine) = super::ec2_query::emulator();
    let launched = launch(&router, "seeding one instance", 1).await;

    // Rows: how the request narrows what it pages — nothing at all, a tag filter that
    // selects the launched instance, and a documented filter that selects none. The floor is
    // a fact of the PARAMETER, so none of them changes the refusal.
    let selections: [(&str, Vec<(String, String)>); 3] = [
        ("no filter", Vec::new()),
        ("a tag filter that selects", probe_filter()),
        (
            "a filter that selects nothing",
            filters(&[("instance-state-name", &["terminated"][..])]),
        ),
    ];

    for (selection, params) in &selections {
        // Rows: one below the published minimum, a single-item page, a page of none, a
        // negative page and one far below — the rule generalises over the value's size and
        // its sign.
        for value in [
            PUBLISHED_PAGE_SIZE_MINIMUM - 1,
            1,
            0,
            -1,
            -PUBLISHED_PAGE_SIZE_MAXIMUM,
        ] {
            let case = format!("{selection} with MaxResults={value}");
            let mut request = params.clone();
            request.push(p("MaxResults", value.to_string()));
            refused(
                &router,
                &case,
                &request,
                "InvalidParameterValue",
                &below_minimum_message(value),
            )
            .await;
        }
        // Rows: the published minimum itself, one above it, and the largest page the
        // operation serves — each served, and each reporting exactly what the selection
        // selects.
        for value in [
            PUBLISHED_PAGE_SIZE_MINIMUM,
            PUBLISHED_PAGE_SIZE_MINIMUM + 1,
            PUBLISHED_PAGE_SIZE_MAXIMUM,
        ] {
            let case = format!("{selection} with MaxResults={value}");
            let mut request = params.clone();
            request.push(p("MaxResults", value.to_string()));
            let body = ok(&router, &case, "DescribeInstances", &request).await;
            let reported = elements(&body, "instanceId");
            let expected: Vec<String> = if *selection == "a filter that selects nothing" {
                Vec::new()
            } else {
                launched.clone()
            };
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?} on the page, got {reported:?}"
            );
        }
    }
}

/// A request that gets BOTH the filter name and the page size wrong is told about the
/// filter: the vocabulary is validated before the page size, so the caller is pointed at the
/// parameter that named something the service does not have.
#[tokio::test]
async fn an_undocumented_filter_is_reported_before_a_page_size_below_the_minimum() {
    let (router, _engine) = super::ec2_query::emulator();
    let _ = launch(&router, "seeding one instance", 1).await;

    for value in [PUBLISHED_PAGE_SIZE_MINIMUM - 1, 0, -1] {
        let case = format!("an undocumented filter with MaxResults={value}");
        let mut request = filters(&[("instance-state", &["running"][..])]);
        request.push(p("MaxResults", value.to_string()));
        refused(
            &router,
            &case,
            &request,
            "InvalidParameterValue",
            &invalid_filter_message("instance-state"),
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// 3. An id selection beside a page size
// ---------------------------------------------------------------------------

/// `InstanceIds` and `MaxResults` select mutually exclusively, so the two together are
/// refused — whatever the ids and whatever the page size — with EC2's sentence naming both
/// parameters by their wire spelling; either on its own is served.
#[tokio::test]
async fn an_id_selection_beside_a_page_size_is_refused_by_its_wire_parameter_names() {
    let (router, _engine) = super::ec2_query::emulator();
    let launched = launch(&router, "seeding two instances", 2).await;
    let absent = "i-0fedcba9876543210";

    // Rows: which ids the request names — one the account holds, several it holds, one it
    // does not, and a mix — so the refusal does not depend on whether the ids resolve.
    let selections: [(&str, Vec<&str>); 4] = [
        ("one held id", vec![launched[0].as_str()]),
        (
            "every held id",
            launched.iter().map(String::as_str).collect(),
        ),
        ("an id the account does not hold", vec![absent]),
        ("a held id beside one it does not", vec![launched[0].as_str(), absent]),
    ];

    for (selection, ids) in &selections {
        // Rows: a page size inside the published range, at each of its bounds, and one below
        // the floor — the combination is refused before any bound on the value is reached.
        for value in [
            PUBLISHED_PAGE_SIZE_MINIMUM,
            PUBLISHED_PAGE_SIZE_MINIMUM + 1,
            PUBLISHED_PAGE_SIZE_MAXIMUM,
            PUBLISHED_PAGE_SIZE_MINIMUM - 1,
        ] {
            let case = format!("{selection} with MaxResults={value}");
            let mut request = super::ec2_query::indexed("InstanceId", ids);
            request.push(p("MaxResults", value.to_string()));
            refused(
                &router,
                &case,
                &request,
                "InvalidParameterCombination",
                ID_SELECTION_WITH_PAGE_SIZE_MESSAGE,
            )
            .await;
        }
    }

    // Either parameter alone is served: the ids the account holds, without a page size, and
    // a page size without ids.
    let ids_only = ok(
        &router,
        "the held ids without a page size",
        "DescribeInstances",
        &super::ec2_query::indexed(
            "InstanceId",
            &launched.iter().map(String::as_str).collect::<Vec<_>>(),
        ),
    )
    .await;
    assert_eq!(
        elements(&ids_only, "instanceId"),
        launched,
        "the held ids without a page size report exactly those instances, got {ids_only}"
    );
    let page_only = ok(
        &router,
        "a page size without ids",
        "DescribeInstances",
        &[p("MaxResults", PUBLISHED_PAGE_SIZE_MINIMUM.to_string())],
    )
    .await;
    assert_eq!(
        elements(&page_only, "instanceId"),
        launched,
        "a page size without ids reports the account's instances, got {page_only}"
    );
}
