// SPDX-License-Identifier: CC0-1.0

//! A filter-only `DescribeInstances` that selects nothing is an ANSWER, not a refusal.
//!
//! The rules under test:
//!
//! 1. "If you specify filters, the output includes information for only those instances that
//!    meet the filter criteria" (`DescribeInstances`) — so a request whose filters no
//!    instance meets is answered with the EMPTY reservation set. "No instance matches" is a
//!    normal result: a client that uses a describe as an existence probe must be able to
//!    tell it from a request the service would not evaluate.
//! 2. One filter carries AT MOST the per-call cap of values EC2 publishes
//!    ([`ec2::state::MAX_FILTER_VALUES`]) — a request AT that cap is EVALUATED, with the
//!    instances its values select and with the empty set when they select none, and only a
//!    request PAST it is refused.
//!
//! The rows are different documented filter names and different value counts — the two
//! things rule 1 generalises over — and one row names a value an instance of the store DOES
//! carry, so an empty answer is never mistaken for a describe that reports nothing at all.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::MAX_FILTER_VALUES;

use super::ec2_query::{call, element, error_code, filters};
use super::wire::{child, child_text, document, set_items};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// An AMI id of the right FORM that this account holds no image for: the launch is served
/// and which image it names is not the subject here.
const AMI: &str = "ami-0123456789abcdef0";

fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// Launch one instance so the store is not empty, and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "RunInstances",
        &[p("ImageId", AMI), p("MinCount", "1"), p("MaxCount", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the launch must be served, got {status} with body {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: expected an instanceId in {body}"))
}

/// The instance ids one `DescribeInstances` reports, having first asserted it was answered
/// with a reservation set at all.
async fn reported(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> Vec<String> {
    let (status, body) = call(router, REGION, "DescribeInstances", params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a filter that selects nothing is an answer, not a refusal, got \
         {status} with body {body}"
    );
    let parsed = document(case, &body);
    assert!(
        parsed
            .root_element()
            .children()
            .any(|node| node.is_element() && node.tag_name().name() == "reservationSet"),
        "case {case:?}: the answer carries the reservation set, empty or not, got {body}"
    );
    parsed
        .descendants()
        .filter(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .filter_map(|instance| child_text(instance, "instanceId"))
        .collect()
}

/// One row of the selection table: the case, the filters the request carries, and the
/// instances they must select.
type SelectionRow = (&'static str, Vec<(String, String)>, Vec<String>);

/// Rule 1 and the serving half of rule 2: filters that select nothing are answered with the
/// empty set, at one value and at the whole per-call cap alike.
#[tokio::test]
async fn filters_that_select_nothing_answer_the_empty_reservation_set() {
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(&router, "the store's one instance").await;

    // Values no instance of the store carries, enough of them to fill the per-call cap.
    let many: Vec<String> = (0..MAX_FILTER_VALUES)
        .map(|index| format!("vpc-ffff{index:013x}"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let addresses: Vec<String> = (0..MAX_FILTER_VALUES)
        .map(|index| format!("198.51.100.{}", index % 256))
        .collect();
    let addresses: Vec<&str> = addresses.iter().map(String::as_str).collect();

    // (case, the filters, the instances they must select)
    let rows: Vec<SelectionRow> = vec![
        (
            "one VPC the account does not hold",
            filters(&[("vpc-id", &["vpc-ffffffffffffffbad"])]),
            Vec::new(),
        ),
        (
            "the value cap of VPCs the account does not hold",
            filters(&[("vpc-id", &many)]),
            Vec::new(),
        ),
        (
            "a tag no instance carries",
            filters(&[("tag:Name", &["nothing-is-called-this"])]),
            Vec::new(),
        ),
        (
            "a private address no instance holds",
            filters(&[("private-ip-address", &addresses)]),
            Vec::new(),
        ),
        (
            "a state no instance of the store is in",
            filters(&[("instance-state-name", &["shutting-down"])]),
            Vec::new(),
        ),
        (
            "two filters, one of which nothing meets",
            filters(&[
                ("image-id", &[AMI]),
                ("tag:Name", &["nothing-is-called-this"]),
            ]),
            Vec::new(),
        ),
        (
            "a filter the store's instance DOES meet",
            filters(&[("image-id", &[AMI])]),
            vec![instance_id.clone()],
        ),
    ];
    for (case, params, expected) in rows {
        let found = reported(&router, case, &params).await;
        assert_eq!(
            found, expected,
            "case {case:?}: expected the instances {expected:?}, got {found:?}"
        );
    }
}

/// The refusing half of rule 2: one value past the per-call cap is refused, and the refusal
/// reports no instance.
#[tokio::test]
async fn one_value_past_the_per_call_cap_is_refused() {
    let (router, _engine) = super::ec2_query::emulator();
    let _ = launch(&router, "the store's one instance").await;

    let past: Vec<String> = (0..=MAX_FILTER_VALUES)
        .map(|index| format!("vpc-ffff{index:013x}"))
        .collect();
    let past: Vec<&str> = past.iter().map(String::as_str).collect();
    let case = "one value past the per-call cap";
    let (status, body) = call(
        &router,
        REGION,
        "DescribeInstances",
        &filters(&[("vpc-id", &past)]),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert!(
        !error_code(&body).is_empty(),
        "case {case:?}: a refusal carries an error code, got {body}"
    );
    let parsed = document(case, &body);
    assert!(
        set_items(parsed.root_element(), "reservationSet").is_empty(),
        "case {case:?}: a refused request reports no instance, got {body}"
    );
}
