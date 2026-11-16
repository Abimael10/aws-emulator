// SPDX-License-Identifier: CC0-1.0

//! How the two managed-node reads REFUSE a request, end to end over the front door a
//! client uses: `DescribeInstanceInformation` and `DescribeInstanceProperties` narrow the
//! same node inventory through the same filter vocabulary, so they classify and word a
//! refused request the same way.
//!
//! Three rules are pinned here, each over a table of inputs rather than over the one
//! request a recorded call happened to send:
//!
//! 1. A filter list is a CONJUNCTION of filters on distinct node attributes
//!    (`smithy_query(query="doc", shape="InstanceInformationStringFilter", member="Key")`
//!    lists the attributes a key may name), so a list naming one key more than once is
//!    refused as a validation of the request, naming the repeated key — whichever key it
//!    is, however many times it repeats, and wherever it sits among the other filters.
//! 2. A filter value whose key selects on an IDENTIFIER the service models an error for is
//!    refused with THAT error rather than with the generic filter-value one: an activation
//!    id outside the modelled `ActivationId` pattern
//!    (`smithy_query(query="shape", shape="ActivationId")`) is `InvalidActivationId`
//!    (`smithy_query(query="shape", shape="InvalidActivationId")`, HTTP 400) naming the
//!    values it rejected. `all/activation_id_filter_values.rs` pins that for the property
//!    describe; the rows here pin it for the node describe, whose own filter member
//!    publishes the same `ActivationIds` key.
//! 3. The paging refusal of a managed-node read is the error code alone: `InvalidNextToken`
//!    models an optional `Message` (`smithy_query(query="shape", shape="InvalidNextToken")`)
//!    and these reads leave it unset, so the body carries no message member at all.
//!
//! The nodes are seeded the way the agent's own registration will write them, through this
//! service's public state API, because registering a node is the agent's call and not part
//! of this service's model.

use serde_json::{json, Map, Value};
use service_com_amazonaws_ssm_amazonssm::ops::types::InstanceProperty;
use service_com_amazonaws_ssm_amazonssm::state::{ComAmazonawsSsmAmazonssmState, ManagedNodeRecord};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router_with_engine, REGION};

/// One of the two managed-node reads, and the members its filters and its report travel
/// in.
#[derive(Clone, Copy, Debug)]
struct NodeApi {
    /// The operation name, as `X-Amz-Target` carries it.
    operation: &'static str,
    /// The member a string filter list goes in.
    filters_member: &'static str,
    /// The member the legacy, operator-less filter list goes in.
    legacy_member: &'static str,
    /// The member a served page lists its nodes in.
    report_member: &'static str,
}

const INFORMATION: NodeApi = NodeApi {
    operation: "DescribeInstanceInformation",
    filters_member: "Filters",
    legacy_member: "InstanceInformationFilterList",
    report_member: "InstanceInformationList",
};

const PROPERTIES: NodeApi = NodeApi {
    operation: "DescribeInstanceProperties",
    filters_member: "FiltersWithOperator",
    legacy_member: "InstancePropertyFilterList",
    report_member: "InstanceProperties",
};

/// Both managed-node reads, so every rule below is asserted on each of them.
const BOTH: [NodeApi; 2] = [INFORMATION, PROPERTIES];

/// The node the seeded inventory holds, in one of the forms the modelled `InstanceId`
/// pattern admits.
const SEEDED_NODE: &str = "mi-0123456789abcdef0";

/// The activation the seeded node registered with: a value of the modelled `ActivationId`
/// shape.
const REGISTERED_ACTIVATION: &str = "0a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d";

/// Another well-formed activation id, which nothing registered with.
const UNUSED_ACTIVATION: &str = "ffffffff-0000-1111-2222-333333333333";

/// A router with this service mounted, holding one managed node that registered with
/// [`REGISTERED_ACTIVATION`] and carries the tags the filter rows select on.
fn seeded_router() -> MultiServiceRouter {
    let (router, engine) = router_with_engine();
    let mut record = ManagedNodeRecord {
        properties: InstanceProperty {
            instance_id: Some(SEEDED_NODE.to_string()),
            activation_id: Some(REGISTERED_ACTIVATION.to_string()),
            resource_type: Some("ManagedInstance".to_string()),
            platform_type: Some("Linux".to_string()),
            ping_status: Some("Online".to_string()),
            ..InstanceProperty::default()
        },
        ..ManagedNodeRecord::default()
    };
    record.tags.insert("Purpose".to_string(), "Test".to_string());
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(REGION, record),
        "a seeded node must carry the node id it is stored under"
    );
    router
}

/// One filter of a request: the key and the values it carries.
type Filter<'a> = (&'a str, &'a [&'a str]);

/// A request carrying `filters` in `api`'s string-filter member.
fn filters_body(api: NodeApi, filters: &[Filter<'_>]) -> Value {
    let list: Vec<Value> = filters
        .iter()
        .map(|(key, values)| json!({ "Key": key, "Values": values }))
        .collect();
    let mut body = Map::new();
    body.insert(api.filters_member.to_string(), Value::Array(list));
    Value::Object(body)
}

/// A request carrying `filters` in `api`'s legacy, operator-less filter member, whose
/// members are the lowercase `key` / `valueSet` pair both legacy lists model.
fn legacy_body(api: NodeApi, filters: &[Filter<'_>]) -> Value {
    let list: Vec<Value> = filters
        .iter()
        .map(|(key, values)| json!({ "key": key, "valueSet": values }))
        .collect();
    let mut body = Map::new();
    body.insert(api.legacy_member.to_string(), Value::Array(list));
    Value::Object(body)
}

/// RULE 1: a filter list naming one key more than once is refused with HTTP 400
/// `ValidationException` naming the repeated key.
///
/// The rows differ along what the rule generalises over: which key repeats, whether it
/// names a property or a tag, how many times it repeats, how many distinct filters the
/// request carries beside it, and where the repetition sits in the list.
#[tokio::test]
async fn a_repeated_filter_key_is_refused_naming_that_key() {
    let router = seeded_router();

    // (case, the filters the request carries, the key the refusal must name)
    let rows: &[(&str, &[Filter<'_>], &str)] = &[
        (
            "the same node-id filter twice",
            &[("InstanceIds", &[SEEDED_NODE]), ("InstanceIds", &[SEEDED_NODE])],
            "InstanceIds",
        ),
        (
            "one node-id filter twice with different values",
            &[
                ("InstanceIds", &[SEEDED_NODE]),
                ("InstanceIds", &["i-01234567"]),
            ],
            "InstanceIds",
        ),
        (
            "a platform filter twice",
            &[("PlatformTypes", &["Linux"]), ("PlatformTypes", &["Windows"])],
            "PlatformTypes",
        ),
        (
            "a ping-status filter three times",
            &[
                ("PingStatus", &["Online"]),
                ("PingStatus", &["Online"]),
                ("PingStatus", &["ConnectionLost"]),
            ],
            "PingStatus",
        ),
        (
            "the repetition after a distinct key",
            &[
                ("ResourceType", &["ManagedInstance"]),
                ("PingStatus", &["Online"]),
                ("ResourceType", &["EC2Instance"]),
            ],
            "ResourceType",
        ),
        (
            "an activation-id filter twice",
            &[
                ("ActivationIds", &[REGISTERED_ACTIVATION]),
                ("ActivationIds", &[UNUSED_ACTIVATION]),
            ],
            "ActivationIds",
        ),
    ];

    // The tag forms only ride in the string-filter member: the legacy list's key is a
    // modelled enum that carries no tag form, so the boundary refuses one there.
    let tag_rows: &[(&str, &[Filter<'_>], &str)] = &[
        (
            "the same tag filter twice",
            &[("tag:Purpose", &["Test"]), ("tag:Purpose", &["Other"])],
            "tag:Purpose",
        ),
        (
            "the tag-key filter twice",
            &[("tag-key", &["Purpose"]), ("tag-key", &["Owner"])],
            "tag-key",
        ),
    ];

    for api in BOTH {
        for (case, filters, repeated) in rows.iter().chain(tag_rows.iter()) {
            let is_tag_row = tag_rows.iter().any(|(name, _, _)| name == case);
            let bodies: Vec<Value> = if is_tag_row {
                vec![filters_body(api, filters)]
            } else {
                vec![filters_body(api, filters), legacy_body(api, filters)]
            };
            for body in bodies {
                let case = format!("{} {case} in {body}", api.operation);
                let (status, parsed) = call(&router, api.operation, &body).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400, got {status} with {parsed}"
                );
                assert_eq!(
                    error_code(&parsed),
                    "ValidationException",
                    "case {case:?}: expected ValidationException, got {parsed}"
                );
                let expected = format!("Cannot specify duplicate filter {repeated}");
                assert_eq!(
                    error_message(&parsed),
                    expected,
                    "case {case:?}: expected {expected:?}, got {:?}",
                    error_message(&parsed)
                );
            }
        }
    }
}

/// RULE 1, the other side: a list whose keys are DISTINCT is served, however many filters
/// it carries and however much their values overlap — what the refusal above is about is a
/// repeated key, not a long list or a repeated value.
#[tokio::test]
async fn distinct_filter_keys_are_served() {
    let router = seeded_router();

    let accepted: &[(&str, &[Filter<'_>])] = &[
        ("one filter", &[("PingStatus", &["Online"])]),
        (
            "two distinct keys",
            &[("PingStatus", &["Online"]), ("PlatformTypes", &["Linux"])],
        ),
        (
            "three distinct keys",
            &[
                ("InstanceIds", &[SEEDED_NODE]),
                ("PlatformTypes", &["Linux"]),
                ("ResourceType", &["ManagedInstance"]),
            ],
        ),
        (
            "one key carrying a repeated value",
            &[("PlatformTypes", &["Linux", "Linux"])],
        ),
        (
            "two distinct keys carrying the same value",
            &[
                ("ActivationIds", &[REGISTERED_ACTIVATION]),
                ("InstanceIds", &[SEEDED_NODE]),
            ],
        ),
    ];

    for api in BOTH {
        for (case, filters) in accepted {
            let body = filters_body(api, filters);
            let case = format!("{} {case} in {body}", api.operation);
            let (status, parsed) = call(&router, api.operation, &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {parsed}"
            );
            let reported = parsed[api.report_member]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: expected a report, got {parsed}"));
            assert_eq!(
                reported.len(),
                1,
                "case {case:?}: expected the one seeded node, got {parsed}"
            );
        }
    }
}

/// RULE 2: an `ActivationIds` filter value outside the modelled `ActivationId` pattern is
/// refused with HTTP 400 `InvalidActivationId` naming the rejected values, on the node
/// describe as well as on the property one.
///
/// The rows differ along which part of the id's shape the value violates (group count,
/// group width, alphabet, case, separator), how many values the filter carries, and where
/// the bad value sits among well-formed ones.
#[tokio::test]
async fn a_malformed_activation_id_filter_value_is_an_activation_id_failure() {
    let router = seeded_router();

    // (case, the values the one filter carries, the values the refusal must name)
    let rows: &[(&str, &[&str], &[&str])] = &[
        (
            "a value that is not an id at all",
            &["not-an-activation"],
            &["not-an-activation"],
        ),
        (
            "four groups instead of five",
            &["0a1b2c3d-4e5f-6a7b-8c9d"],
            &["0a1b2c3d-4e5f-6a7b-8c9d"],
        ),
        (
            "a first group one character short",
            &["0a1b2c3-4e5f-6a7b-8c9d-0e1f2a3b4c5d"],
            &["0a1b2c3-4e5f-6a7b-8c9d-0e1f2a3b4c5d"],
        ),
        (
            "a character outside the hexadecimal alphabet",
            &["0a1b2c3g-4e5f-6a7b-8c9d-0e1f2a3b4c5d"],
            &["0a1b2c3g-4e5f-6a7b-8c9d-0e1f2a3b4c5d"],
        ),
        (
            "the id in upper case",
            &["0A1B2C3D-4E5F-6A7B-8C9D-0E1F2A3B4C5D"],
            &["0A1B2C3D-4E5F-6A7B-8C9D-0E1F2A3B4C5D"],
        ),
        (
            "the groups undelimited",
            &["0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d"],
            &["0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d"],
        ),
        (
            "a malformed value after a well-formed one",
            &[REGISTERED_ACTIVATION, "not-an-activation"],
            &["not-an-activation"],
        ),
        (
            "two malformed values among well-formed ones",
            &[REGISTERED_ACTIVATION, "first-bad", UNUSED_ACTIVATION, "second-bad"],
            &["first-bad", "second-bad"],
        ),
    ];

    for api in BOTH {
        for (case, values, rejected) in rows {
            for body in [
                filters_body(api, &[("ActivationIds", values)]),
                legacy_body(api, &[("ActivationIds", values)]),
            ] {
                let case = format!("{} {case} in {body}", api.operation);
                let (status, parsed) = call(&router, api.operation, &body).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400, got {status} with {parsed}"
                );
                assert_eq!(
                    error_code(&parsed),
                    "InvalidActivationId",
                    "case {case:?}: expected InvalidActivationId, got {parsed}"
                );
                let expected = format!("Invalid Activation Ids {}", rejected.join(", "));
                assert_eq!(
                    error_message(&parsed),
                    expected,
                    "case {case:?}: expected {expected:?}, got {:?}",
                    error_message(&parsed)
                );
            }
        }
    }
}

/// RULE 2, the other side: a value the pattern ADMITS is served — the nodes that
/// registered with that activation, and an empty page for an activation nothing registered
/// with, which is how a caller asks whether anything registered under it.
#[tokio::test]
async fn a_well_formed_activation_id_filter_value_is_served() {
    let router = seeded_router();

    // (case, the values the filter carries, how many nodes the store answers with)
    let rows: &[(&str, &[&str], usize)] = &[
        (
            "the activation the seeded node registered with",
            &[REGISTERED_ACTIVATION],
            1,
        ),
        (
            "a well-formed activation nothing registered with",
            &[UNUSED_ACTIVATION],
            0,
        ),
        (
            "both, which a filter's values are alternatives of",
            &[REGISTERED_ACTIVATION, UNUSED_ACTIVATION],
            1,
        ),
        ("an id of nothing but zeroes", &["00000000-0000-0000-0000-000000000000"], 0),
    ];

    for api in BOTH {
        for (case, values, expected) in rows {
            let body = filters_body(api, &[("ActivationIds", values)]);
            let case = format!("{} {case} in {body}", api.operation);
            let (status, parsed) = call(&router, api.operation, &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {parsed}"
            );
            let reported = parsed[api.report_member]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: expected a report, got {parsed}"));
            assert_eq!(
                reported.len(),
                *expected,
                "case {case:?}: expected {expected} node(s), got {parsed}"
            );
        }
    }
}

/// RULE 3: a `NextToken` neither read issued is refused with HTTP 400 `InvalidNextToken`
/// whose body carries NO message member — under either spelling the protocols use — so a
/// client sees the error code and nothing the service never sent.
///
/// The rows span the forms a token a client might hold can take: prose, an opaque base64
/// blob, a number, a truncated node id and a page cursor of another read.
#[tokio::test]
async fn an_unissued_paging_token_is_refused_with_no_message() {
    let router = seeded_router();

    let tokens: &[(&str, &str)] = &[
        ("prose", "not-a-token"),
        ("an opaque base64 blob", "QUFBQUFBQUFBQUFBQUE9PQ=="),
        ("a number", "1"),
        ("a truncated node id", "i-"),
        ("a node id of the wrong length", "i-0123456789abcdef"),
        ("another read's cursor", "/aws/service/ami-amazon-linux-latest"),
    ];

    for api in BOTH {
        for (case, token) in tokens {
            let body = json!({ "NextToken": token });
            let case = format!("{} {case} ({token:?})", api.operation);
            let (status, parsed) = call(&router, api.operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {parsed}"
            );
            assert_eq!(
                error_code(&parsed),
                "InvalidNextToken",
                "case {case:?}: expected InvalidNextToken, got {parsed}"
            );
            for member in ["Message", "message"] {
                assert!(
                    parsed.get(member).is_none(),
                    "case {case:?}: the body must carry no {member} member, got {parsed}"
                );
            }
        }
    }
}

/// RULE 3, the other side: the cursor these reads DO issue is accepted, so the refusal
/// above is about a token the service never handed out rather than about paging itself.
#[tokio::test]
async fn a_cursor_these_reads_issue_is_accepted() {
    let router = seeded_router();

    for api in BOTH {
        // The cursor is the id of the node the next page starts at, so the seeded node's
        // own id is a token either read would hand out.
        let body = json!({ "NextToken": SEEDED_NODE });
        let case = format!("{} resuming at {SEEDED_NODE:?}", api.operation);
        let (status, parsed) = call(&router, api.operation, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {parsed}"
        );
        let reported = parsed[api.report_member]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a report, got {parsed}"));
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected the one seeded node, got {parsed}"
        );
    }
}
