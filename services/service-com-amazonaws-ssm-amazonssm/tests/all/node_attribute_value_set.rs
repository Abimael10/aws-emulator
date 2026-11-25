// SPDX-License-Identifier: CC0-1.0

//! The value set a refusal STATES for the attribute a node summary groups its counts by.
//!
//! RULE: a value outside an enumerated type is refused `ValidationException` whose clause
//! names the values the live SERVICE accepts for that type — not the values this checkout's
//! model happens to declare, where the model is behind the service — listed in the one order
//! this service lists a set in (its own hash table, bucket by bucket). The set and its order
//! belong to the TYPE, so neither depends on which member carried the value, how deep in the
//! request it sat, what else the request asked for, or how the submitted value missed the
//! set. Only the rejected value and the member path come from the request.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router — so the sentence asserted here is the one a client
//! reads.
//!
//! The rows differ along what the rule generalises over: the FORM the submitted value misses
//! the set in, the POSITION the offending aggregator sits at (the first of a list, the
//! second, and one nested inside another), and the members the request carries beside it —
//! rather than the single call a recorded unit happened to make.
//!
//! What this suite does NOT pin is ADMISSION: which values a member accepts is the `@enum`
//! constraint of the generated input schema, built from the checked-in model, so a value the
//! live service accepts and this model does not declare stays refused until the model is
//! revised. The last test pins the other half of that — every value the model DOES declare
//! is admitted and served — so a refusal above cannot pass by refusing everything.

use serde_json::{json, Value};
use service_common::validation_message::hashed_enum_order;
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// The 11 values the live service LISTS for that attribute, in the order its refusal lists
/// them: the bracketed sequence of the message, in full (recorded behaviour of
/// `ListNodesSummary` refusing an `AttributeName` outside the set).
const LISTED: &[&str] = &[
    "PlatformVersion",
    "AvailabilityZone",
    "ResourceType",
    "PlatformType",
    "ExtensionStatus",
    "AgentVersion",
    "Region",
    "SourceType",
    "ExtensionVersion",
    "ExtensionId",
    "PlatformName",
];

/// The eight values this checkout's model declares for the type
/// (`smithy_query(query="shape",shape="NodeAttributeName")`), which is what a caller may
/// actually aggregate by here.
const DECLARED: &[&str] = &[
    "AgentVersion",
    "PlatformName",
    "PlatformType",
    "PlatformVersion",
    "Region",
    "ResourceType",
    "SourceType",
    "AvailabilityZone",
];

/// The three values the live service accepts that this checkout's model does not declare:
/// the attributes of a node's extensions.
const BEYOND_THE_MODEL: &[&str] = &["ExtensionStatus", "ExtensionVersion", "ExtensionId"];

/// The only `AggregatorType` the model declares (`smithy_query(query="shape",
/// shape="NodeAggregatorType")`: `Count`).
const AGGREGATOR_TYPE: &str = "Count";

/// The only `NodeTypeName` it declares (`smithy_query(query="shape",
/// shape="NodeTypeName")`: `Instance`).
const TYPE_NAME: &str = "Instance";

/// The name of the resource data sync the integrated experience puts in an account: "the
/// name of the supported resource data sync is AWS-QuickSetup-ManagedNode"
/// (`smithy_query(query="doc", shape="ListNodesSummaryRequest", member="SyncName")`).
const MANAGED_NODE_SYNC: &str = "AWS-QuickSetup-ManagedNode";

/// One aggregator naming `attribute`, as the request body writes it.
fn aggregator(attribute: &str) -> Value {
    json!({
        "AggregatorType": AGGREGATOR_TYPE,
        "TypeName": TYPE_NAME,
        "AttributeName": attribute,
    })
}

/// One aggregator by `attribute` refined by an aggregator by `nested`.
fn nested_aggregator(attribute: &str, nested: &str) -> Value {
    let mut outer = aggregator(attribute);
    outer["Aggregators"] = json!([aggregator(nested)]);
    outer
}

/// The whole message a refusal of `value` at `path` carries, with the set the service
/// accepts listed as the service lists it.
fn expected_message(value: &str, path: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy constraint: \
         Member must satisfy enum value set: [{}]",
        LISTED.join(", ")
    )
}

/// `ListNodesSummary` at [`REGION`]'s endpoint.
async fn summarise(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call_in_region(router, REGION, "ListNodesSummary", body).await
}

/// Turn the integrated experience on at [`REGION`]'s endpoint through this service's own
/// `CreateResourceDataSync`, which is what puts the managed node sync in the account.
async fn enable_experience(router: &MultiServiceRouter) {
    let (status, body) = call_in_region(
        router,
        REGION,
        "CreateResourceDataSync",
        &json!({
            "SyncName": MANAGED_NODE_SYNC,
            "SyncType": "SyncFromSource",
            "SyncSource": {
                "SourceType": "SingleAccountMultiRegions",
                "SourceRegions": [REGION],
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "creating the managed node sync must succeed, got {status} with {body}"
    );
}

/// Values a client can submit for the attribute that the enumerated type does not admit,
/// differing in HOW they miss it: a name no attribute spells, a declared value with a
/// suffix, a declared value whose case differs, a declared value with surrounding space, a
/// value of another enumerated type of this model, and no text at all.
fn values_outside_the_set() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a name no node attribute spells", "NotANodeAttribute"),
        ("a declared value with a suffix", "PlatformTypeName"),
        ("a declared value in the wrong case", "platformtype"),
        ("a declared value with surrounding space", " Region "),
        ("a value of another enumerated type", "InstanceId"),
        ("no text at all", ""),
    ]
}

/// One row of the position table: the case it names, the `Aggregators` a request carrying
/// the offending value sends, and the member path the clause names it at.
type Position = (&'static str, fn(&str) -> Value, &'static str);

/// Where in one request an aggregator can carry the offending attribute: the `Aggregators`
/// the request sends, and the member path the clause names.
///
/// `NodeAggregatorList` holds one or two aggregators and an aggregator refines another one
/// with an aggregator of the same shape, so a value outside the type can sit at the first
/// position, the second, or inside the first.
fn positions() -> Vec<Position> {
    vec![
        (
            "the only aggregator",
            |value: &str| json!([aggregator(value)]),
            "aggregators.1.member.attributeName",
        ),
        (
            "the second of two aggregators",
            |value: &str| json!([aggregator("Region"), aggregator(value)]),
            "aggregators.2.member.attributeName",
        ),
        (
            "an aggregator nested inside another",
            |value: &str| json!([nested_aggregator("Region", value)]),
            "aggregators.1.member.aggregators.1.member.attributeName",
        ),
    ]
}

/// The members a summary can carry beside the offending aggregator, which decide nothing
/// about the set a refusal states.
fn request_shapes() -> Vec<(&'static str, Vec<(&'static str, Value)>)> {
    vec![
        ("the aggregators alone", Vec::new()),
        ("a page size as well", vec![("MaxResults", json!(1))]),
        (
            "a filter as well",
            vec![(
                "Filters",
                json!([{ "Key": "PlatformType", "Values": ["Linux"] }]),
            )],
        ),
        (
            "the supported sync name as well",
            vec![("SyncName", json!(MANAGED_NODE_SYNC))],
        ),
    ]
}

/// RULE: whatever form the value missed the set in, wherever the aggregator sat and whatever
/// else the request carried, the clause states the set the service accepts in the order it
/// lists it — and the refusal precedes the account's own gate, so it is the same answer
/// before and after the integrated experience is enabled.
#[tokio::test]
async fn a_value_outside_the_attribute_is_refused_with_the_accepted_set() {
    let router = router();

    for enabled in [false, true] {
        if enabled {
            enable_experience(&router).await;
        }
        for (value_case, rejected) in values_outside_the_set() {
            for (position_case, aggregators, path) in positions() {
                for (shape_case, extra) in request_shapes() {
                    let case = format!(
                        "{value_case}, at {position_case}, with {shape_case} (experience \
                         enabled: {enabled})"
                    );
                    let mut body = json!({ "Aggregators": aggregators(rejected) });
                    for (member, value) in &extra {
                        body[*member] = value.clone();
                    }
                    let (status, response) = summarise(&router, &body).await;
                    assert_eq!(
                        (status, error_code(&response).as_str()),
                        (400, "ValidationException"),
                        "case {case:?}: a value outside the type is refused, got {status} with \
                         {response}"
                    );
                    let expected = expected_message(rejected, path);
                    let stated = error_message(&response);
                    assert_eq!(
                        stated, expected,
                        "case {case:?}: expected {expected:?}, got {stated:?}"
                    );
                }
            }
        }
    }
}

/// RULE, on the SET and its ORDER: the listing holds every value the model declares plus the
/// three the service accepts beyond it, and lists them in the order this service's hash table
/// is walked — which is neither the model's declaration order nor a sort of the set.
#[tokio::test]
async fn the_listing_is_the_accepted_set_in_this_services_order() {
    // The MEMBERS: the declared values and the three beyond them, and nothing else.
    let mut accepted: Vec<&str> = DECLARED.to_vec();
    accepted.extend_from_slice(BEYOND_THE_MODEL);
    let mut expected_members = accepted.clone();
    expected_members.sort_unstable();
    let mut listed_members = LISTED.to_vec();
    listed_members.sort_unstable();
    assert_eq!(
        listed_members, expected_members,
        "case the stated set's members: expected {expected_members:?}, got {listed_members:?}"
    );

    // The ORDER: the hash table's, computed from the values themselves, so the same set is
    // listed alike whichever order it is presented in.
    for (case, presented) in [
        (
            "the model's declaration order, then the three beyond it",
            accepted.clone(),
        ),
        ("that order reversed", {
            let mut reversed = accepted.clone();
            reversed.reverse();
            reversed
        }),
        ("the set sorted", expected_members.clone()),
    ] {
        let derived = hashed_enum_order(&presented);
        assert_eq!(
            derived, LISTED,
            "case presented in {case:?}: expected the listing {LISTED:?}, got {derived:?}"
        );
    }

    // And it is NEITHER of the two orders a reader might take it for, so a regression to a
    // plain rendering of the set fails here.
    assert_ne!(
        LISTED, &accepted[..],
        "case the declaration order: the listing must not be the model's own order"
    );
    assert_ne!(
        LISTED,
        &expected_members[..],
        "case a sort of the set: the listing must not be the set sorted"
    );

    // Every value beyond the model is listed, which is the half the model cannot supply.
    let stated = LISTED.join(", ");
    for value in BEYOND_THE_MODEL {
        assert!(
            LISTED.contains(value),
            "case {value:?}: the listing must name the value the service accepts, got {stated:?}"
        );
    }
}

/// RULE, the other half: every value the model DECLARES is admitted — the request is served
/// rather than refused — so the refusals above cannot pass by refusing every value.
#[tokio::test]
async fn every_declared_attribute_is_admitted_and_served() {
    let router = router();
    enable_experience(&router).await;

    for attribute in DECLARED {
        let (status, response) =
            summarise(&router, &json!({ "Aggregators": [aggregator(attribute)] })).await;
        assert_eq!(
            status, 200,
            "case {attribute:?}: an attribute the model declares must be served, got {status} \
             with {response}"
        );
        assert!(
            response["Summary"].is_array(),
            "case {attribute:?}: the answer must carry a summary, got {response}"
        );

        // The same attribute nested inside another aggregator is admitted too, so admission
        // is the member's type at any depth.
        let (nested_status, nested) = summarise(
            &router,
            &json!({ "Aggregators": [nested_aggregator("Region", attribute)] }),
        )
        .await;
        assert_eq!(
            nested_status, 200,
            "case {attribute:?} nested: it must be served too, got {nested_status} with {nested}"
        );
    }
}
