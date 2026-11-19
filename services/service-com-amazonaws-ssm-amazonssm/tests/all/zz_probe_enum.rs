// SPDX-License-Identifier: CC0-1.0

//! The WORDING of `DescribeMaintenanceWindowsForTarget`'s boundary refusal: the clauses of
//! this read state the member and the requirement it broke and NOTHING of what the caller
//! sent, whichever modelled constraint the request broke.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router — so the sentence asserted here is the one a client reads.
//!
//! The rules under test:
//!
//! 1. **The decision belongs to the OPERATION, not to the member or its type.** Every clause
//!    of this read is stated `Value at '<member path>' …`: a `ResourceType` outside
//!    `MaintenanceWindowResourceType`, a required member the request left out, and a
//!    `MaxResults` under the modelled minimum alike. The rows differ along the constraint
//!    class broken and along which member broke it.
//! 2. **A sibling operation carrying the SAME enumerated type quotes the value it refused**,
//!    which is what makes rule 1 a rule about this operation rather than about the type: the
//!    register of a window's targets states `Value '<value>' at 'resourceType' …` over the
//!    same value set.

use serde_json::{json, Value};

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// `MaintenanceWindowResourceType`'s values as this service lists them in a refusal: each
/// value sits in the bucket its own text hashes to
/// (`service_common::validation_message::enum_value_bucket`, this emulator's one statement of
/// that order) and the set is listed bucket by bucket, which for these two is
/// `RESOURCE_GROUP` (14) before `INSTANCE` (21) — not the order the model declares them in
/// (`smithy_query(query="shape", shape="MaintenanceWindowResourceType")`: `INSTANCE`,
/// `RESOURCE_GROUP`).
const RESOURCE_TYPE_VALUE_SET: &str = "RESOURCE_GROUP, INSTANCE";

/// One target criterion, so a case breaks ONE constraint.
fn a_criterion() -> Value {
    json!([{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }])
}

/// The whole message a refusal over one broken constraint carries.
fn one_violation(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// RULE 1: every clause of this read states the member and the requirement alone.
#[tokio::test]
async fn the_refusal_of_this_read_states_no_value() {
    let router = router();

    // (case, the request, the clause the refusal states)
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a resource type outside the enumerated type",
            json!({ "ResourceType": "NOT_A_RESOURCE_TYPE", "Targets": a_criterion() }),
            format!(
                "Value at 'resourceType' failed to satisfy constraint: Member must satisfy \
                 enum value set: [{RESOURCE_TYPE_VALUE_SET}]"
            ),
        ),
        (
            "the right value in the wrong case, which is outside it too",
            json!({ "ResourceType": "instance", "Targets": a_criterion() }),
            format!(
                "Value at 'resourceType' failed to satisfy constraint: Member must satisfy \
                 enum value set: [{RESOURCE_TYPE_VALUE_SET}]"
            ),
        ),
        (
            "the same member absent altogether",
            json!({ "Targets": a_criterion() }),
            "Value at 'resourceType' failed to satisfy constraint: Member must not be null"
                .to_string(),
        ),
        (
            "the other required member absent",
            json!({ "ResourceType": "INSTANCE" }),
            "Value at 'targets' failed to satisfy constraint: Member must not be null"
                .to_string(),
        ),
        (
            "a page size under the modelled minimum",
            json!({
                "ResourceType": "INSTANCE",
                "Targets": a_criterion(),
                "MaxResults": 0,
            }),
            "Value at 'maxResults' failed to satisfy constraint: Member must have value \
             greater than or equal to 1"
                .to_string(),
        ),
    ];

    for (case, body, clause) in rows {
        let (status, response) = call_in_region(
            &router,
            REGION,
            "DescribeMaintenanceWindowsForTarget",
            &body,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the request is refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: the refusal is a ValidationException, got {response}"
        );
        let expected = one_violation(&clause);
        let stated = error_message(&response);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

/// RULE 2: the register of a window's targets carries the same enumerated type and QUOTES
/// the value it refused, so what rule 1 pins belongs to the read rather than to the type.
#[tokio::test]
async fn a_sibling_operation_over_the_same_type_quotes_the_value() {
    let router = router();
    let case = "the register of a window's target";
    // A `WindowId` of the form the modelled pattern requires, so the resource type is the
    // one constraint this request breaks.
    let (status, response) = call_in_region(
        &router,
        REGION,
        "RegisterTargetWithMaintenanceWindow",
        &json!({
            "WindowId": "mw-00000000000000000",
            "ResourceType": "NOT_A_RESOURCE_TYPE",
            "Targets": a_criterion(),
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the request is refused, got {status} {response}"
    );
    let expected = one_violation(&format!(
        "Value 'NOT_A_RESOURCE_TYPE' at 'resourceType' failed to satisfy constraint: Member \
         must satisfy enum value set: [{RESOURCE_TYPE_VALUE_SET}]"
    ));
    let stated = error_message(&response);
    assert_eq!(
        stated, expected,
        "case {case:?}: expected {expected:?}, got {stated:?}"
    );
}
